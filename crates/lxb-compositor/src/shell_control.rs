//! The compositor half of `lxb_shell_v1`.
//!
//! Two things a console-style session needs cannot be expressed in
//! layer-shell. The first is the guide button: while a fullscreen application
//! owns the keyboard the shell receives no key events at all, which is exactly
//! the moment the user needs a way back out of it, so the compositor has to
//! route that one binding itself. The second is closing that application, which
//! only the compositor can ask for politely.

use lxb_protocol::server::lxb_shell_v1::{self, LxbShellV1};
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::Transform;
use smithay::wayland::compositor::with_states;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

use crate::input::window_accepts_keyboard_focus;
use crate::outputs::DisplayMode;
use crate::state::LxbState;
use crate::teardown;

/// One window as the overview describes it: the id the shell can activate it
/// by, its title, and its logical size (for aspect-fitting the card frame).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewEntry {
    pub id: u32,
    pub title: String,
    /// What the application calls itself, empty when it says nothing. Part of
    /// the entry rather than a separate list because it is diffed with the
    /// rest: a window that changes its name is a window list that changed.
    pub app_id: String,
    pub width: u32,
    pub height: u32,
}

/// Tracks every shell bound to the protocol, plus the state they were last
/// told about.
#[derive(Debug)]
pub struct ShellControlState {
    #[allow(dead_code)]
    global: GlobalId,
    instances: Vec<LxbShellV1>,
    /// Shells bound so recently that they may not have their `wl_output`s
    /// yet. Per-display events name an output, so one sent before the client
    /// has bound any reaches nobody — and the caches below would then record
    /// it as delivered and never send it again, leaving a shell that started
    /// while applications were already running convinced nothing is. They
    /// are resent, in full, until one lands.
    awaiting_outputs: Vec<LxbShellV1>,
    /// Last session-wide title broadcast, so an unchanged foreground sends
    /// nothing. Only reaches clients older than version 3.
    foreground: String,
    /// Last title broadcast per display, likewise. Rebuilt from the live
    /// outputs on every refresh, so a display going away drops out of it.
    output_foreground: Vec<(Output, String)>,
    /// Last application identity broadcast per display. Separate from the
    /// title because it answers a different question: the title is what a menu
    /// prints, this is what a setting is filed under.
    output_app_id: Vec<(Output, String)>,
    /// Last window list broadcast per display, likewise.
    output_windows: Vec<(Output, Vec<OverviewEntry>)>,
    /// Last HDR status broadcast per display. Diffed like the rest, because
    /// what it reports — whether the connector is in HDR — changes only when
    /// somebody asks it to, and a shell redrawing its Settings column on every
    /// frame of an unchanged display would be redrawing it for nothing.
    output_hdr: Vec<(Output, crate::hdr::Status)>,
    /// Last mode list broadcast per display, diffed for the same reason and
    /// with more at stake: a connector offers dozens of modes, and resending
    /// them all whenever a window moved would be a batch of events per frame.
    output_modes: Vec<(Output, Vec<DisplayMode>)>,
    /// Last orientation broadcast per display, for the displays this
    /// compositor turns itself. Diffed like the rest; a display missing from
    /// it is one whose picture is not ours to turn, and no event is sent for
    /// it at all.
    output_transform: Vec<(Output, Transform)>,
    /// Display the shell says the user is on, from `set_launch_output`.
    launch_output: Option<Output>,
    /// Questions in flight: who asked, and what number they gave it.
    ///
    /// One list rather than one per client, because an answer names only the
    /// question — the shell has no idea who is behind it, and should not: what
    /// it is answering is "may this application see a screen", not "reply to
    /// that client".
    asked: Vec<Question>,
}

/// One application waiting to be told whether it may see a display.
#[derive(Debug)]
struct Question {
    /// The client that asked, and the number *it* gave the question. A second
    /// client may be using the same number for a different question, so both
    /// halves are needed to name one.
    asker: LxbShellV1,
    id: u32,
}

/// First version that reports the foreground application per display. Below
/// it, a shell only learns about the session as a whole.
const PER_OUTPUT_SINCE: u32 = 3;

/// First version with the window overview: per-display window lists, and the
/// requests to enter it and to activate a window from it.
const OVERVIEW_SINCE: u32 = 4;

/// First version that forwards the on-screen keyboard binding. Below it a
/// shell has no way to hear that key, and its keyboard can only be summoned
/// from a controller.
const KEYBOARD_SINCE: u32 = 6;

/// First version with the stick pointer: the two requests that move and click
/// the seat's pointer, and the per-display application identity a shell files
/// the choice to turn it on under.
const POINTER_SINCE: u32 = 7;

/// First version that can put a display into high dynamic range, and that says
/// which displays are capable of it. Below it a shell has no way to ask, and
/// the compositor still honours whatever the config file set.
const HDR_SINCE: u32 = 9;

/// First version that says which of the HDR settings a display can honour, as
/// opposed to merely whether it can be driven in HDR at all.
const HDR_CONTROLS_SINCE: u32 = 10;

/// First version that lists what each display can be driven at, and can be
/// asked to change it. Below it a shell has no way to know a connector offers
/// more than the mode it is on, and the compositor drives whatever its own
/// config asked for.
const MODES_SINCE: u32 = 11;

/// First version a shell can put the cursor away with. Below it the compositor
/// still hides the cursor for its own keyboard; what it cannot be told about
/// is the controller, which it never sees.
///
/// Nothing gates on it — the request is simply there or not — but the ladder of
/// versions is kept whole here, so that what each one added can be read off in
/// one place.
#[allow(dead_code)]
const HIDE_POINTER_SINCE: u32 = 12;

/// First version that names the application behind each listed window, not
/// only the one in front. Below it a shell can tell that windows exist but not
/// what any of them is, so it cannot know that the application a user just
/// picked is the one already running.
const WINDOW_APP_ID_SINCE: u32 = 13;

/// First version that can fly a window back out of the tile it was asked for
/// on. Below it a shell can still raise the window — it simply appears,
/// without the flight.
#[allow(dead_code)]
const RESTORE_SINCE: u32 = 14;

/// First version that can move a window to another display and photograph one.
/// Below it neither is possible from a client at all: the layout and the
/// pixels both belong to the compositor.
const WINDOW_MOVE_AND_CAPTURE_SINCE: u32 = 15;

/// First version that says how each display's picture is turned, and can be
/// asked to turn it. Below it a shell has no way to know a display is standing
/// on its side, and the compositor draws whatever its own config asked for.
const TRANSFORM_SINCE: u32 = 16;

/// First version that can photograph a whole display, and that forwards the
/// screenshot binding. Below it the only picture a shell can ask for is of one
/// window, and the key does nothing at all.
const SCREENSHOT_SINCE: u32 = 17;

/// First version that can put a question to the shell on another client's
/// behalf: the desktop portal asking whether an application may see a display.
/// Below it there is nowhere to ask, and the portal refuses rather than
/// sharing a screen nobody agreed to.
const SHARE_SINCE: u32 = 18;

/// First version that can run an application without ever showing it. Below
/// it, Valve's client puts its own windows over the shell's loading screen and
/// there is nothing the shell can do about it.
const OUT_OF_SIGHT_SINCE: u32 = 19;

/// First version that can warm a display's picture, and that says which
/// displays have a gamma ramp to warm. Below it the shell's Night light page
/// still remembers what it was set to — the file is read by whichever
/// compositor comes next — but nothing is sent and no display reports being
/// able to do it.
const NIGHT_LIGHT_SINCE: u32 = 20;

/// The version advertised, and so the highest a shell can bind. Every request
/// below it is still served, so an older shell keeps working.
const CURRENT_VERSION: u32 = NIGHT_LIGHT_SINCE;

/// Each constant above names the one feature that arrived in its version, and
/// the numbers only ever go up by one. Said here so that two branches each
/// claiming "the next version" cannot both be merged — the easy mistake, and
/// one that otherwise shows up as a shell silently not being sent an event.
const _: () = assert!(NIGHT_LIGHT_SINCE == OUT_OF_SIGHT_SINCE + 1);

impl ShellControlState {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<LxbShellV1, ()> + 'static,
    {
        Self {
            global: display.create_global::<D, LxbShellV1, _>(CURRENT_VERSION, ()),
            instances: Vec::new(),
            awaiting_outputs: Vec::new(),
            foreground: String::new(),
            output_foreground: Vec::new(),
            output_app_id: Vec::new(),
            output_windows: Vec::new(),
            output_hdr: Vec::new(),
            output_modes: Vec::new(),
            output_transform: Vec::new(),
            launch_output: None,
            asked: Vec::new(),
        }
    }

    /// Whether a shell is listening. Used only to explain an inert binding.
    pub fn has_shell(&self) -> bool {
        !self.instances.is_empty()
    }

    pub fn launch_output(&self) -> Option<&Output> {
        self.launch_output.as_ref()
    }

    /// Whether `client` is the session shell — the one that bound
    /// `lxb_shell_v1`.
    ///
    /// Asked when two clients want the same thing and only the shell may have
    /// it. Binding this protocol is what makes a client the shell, so it is
    /// also the only honest way to tell it apart from an application.
    pub fn is_shell_client(&self, client: &Client) -> bool {
        self.instances.iter().any(|instance| {
            instance
                .client()
                .is_some_and(|owner| owner.id() == client.id())
        })
    }

    fn send_guide(&self) {
        for instance in &self.instances {
            instance.guide();
        }
    }

    /// Whether any shell listening is new enough to be told about the
    /// keyboard binding. Used only to explain a key that did nothing.
    fn wants_keyboard(&self) -> bool {
        self.instances
            .iter()
            .any(|instance| instance.version() >= KEYBOARD_SINCE)
    }

    fn send_keyboard(&self) {
        for instance in &self.instances {
            if instance.version() >= KEYBOARD_SINCE {
                instance.keyboard();
            }
        }
    }

    /// Whether any shell listening can be asked for a screenshot. Used only to
    /// explain a key that did nothing.
    fn wants_screenshot(&self) -> bool {
        self.instances
            .iter()
            .any(|instance| instance.version() >= SCREENSHOT_SINCE)
    }

    /// Tell every shell that the screenshot binding fired, and on which
    /// display. Whether it reached anybody, so a key pressed on a display the
    /// shell has never bound is not silently dropped.
    fn send_screenshot(&self, output: &Output) -> bool {
        let mut sent = false;
        for instance in &self.instances {
            if instance.version() < SCREENSHOT_SINCE {
                continue;
            }
            let Some(client) = instance.client() else {
                continue;
            };
            for wl_output in output.client_outputs(&client) {
                instance.screenshot(&wl_output);
                sent = true;
            }
        }
        sent
    }

    /// Put one client's question to everybody else bound to this interface,
    /// which in a running session is the shell.
    ///
    /// Never back to the asker: the portal binds this interface too, and a
    /// question that came back to the client that asked it would be a portal
    /// answering itself.
    fn send_share_request(&mut self, asker: &LxbShellV1, id: u32, app_id: &str) -> bool {
        let mut asked = false;
        for instance in &self.instances {
            if instance == asker || instance.version() < SHARE_SINCE {
                continue;
            }
            instance.share_request(id, app_id.to_string());
            asked = true;
        }
        if asked {
            self.asked.push(Question {
                asker: asker.clone(),
                id,
            });
        }
        asked
    }

    /// Tell whoever asked what the shell decided.
    ///
    /// The display is resolved into the asking client's *own* `wl_output`: the
    /// one the shell answered with belongs to the shell, and an object from one
    /// client means nothing to another.
    fn send_share_answer(&mut self, id: u32, output: Option<&Output>) {
        // The first question with this number, which is the oldest outstanding
        // one. A shell answering out of order is answering the wrong question,
        // and that cannot be told apart from here — but it can be kept from
        // answering the same one twice.
        let Some(index) = self.asked.iter().position(|question| question.id == id) else {
            return;
        };
        let question = self.asked.remove(index);
        let resolved = output.and_then(|output| {
            let client = question.asker.client()?;
            output.client_outputs(&client).next()
        });
        question.asker.share_answered(id, resolved.as_ref());
    }

    /// Forget the questions of a client that has gone. There is nobody left to
    /// tell, and the answer was only ever for them.
    fn forget_shares(&mut self, gone: &LxbShellV1) {
        self.asked.retain(|question| &question.asker != gone);
    }

    /// Refuse everything outstanding: the shell that was going to answer has
    /// gone, and a question nobody can answer is a no.
    fn refuse_all_shares(&mut self) {
        for question in std::mem::take(&mut self.asked) {
            question.asker.share_answered(question.id, None);
        }
    }

    fn broadcast_foreground(&mut self, title: String) {
        if self.foreground == title {
            return;
        }
        self.foreground = title;
        for instance in &self.instances {
            // Version 3 shells are told per display instead; sending both
            // would leave them two answers to the same question.
            if instance.version() < PER_OUTPUT_SINCE {
                instance.foreground(self.foreground.clone());
            }
        }
    }

    /// Publish the per-display titles, sending only what changed.
    ///
    /// `current` is the whole picture, so displays that have gone away simply
    /// stop being listed.
    fn broadcast_output_foreground(&mut self, current: Vec<(Output, String)>) {
        for (output, title) in &current {
            let known = self
                .output_foreground
                .iter()
                .any(|(seen, seen_title)| seen == output && seen_title == title);
            if known {
                continue;
            }
            for instance in &self.instances {
                send_output_foreground(instance, output, title);
            }
        }
        self.output_foreground = current;
    }

    /// The same for the per-display application identities.
    ///
    /// Diffed separately from the titles rather than sent with them: a title
    /// changes every time a document is saved or a track starts, and the
    /// identity behind it does not.
    fn broadcast_output_app_id(&mut self, current: Vec<(Output, String)>) {
        for (output, app_id) in &current {
            let known = self
                .output_app_id
                .iter()
                .any(|(seen, seen_id)| seen == output && seen_id == app_id);
            if known {
                continue;
            }
            for instance in &self.instances {
                send_output_app_id(instance, output, app_id);
            }
        }
        self.output_app_id = current;
    }

    /// Publish the per-display window lists, resending only displays whose
    /// list actually changed. Order is topmost first — the same order the
    /// card layout assigns slots in.
    fn broadcast_output_windows(&mut self, current: Vec<(Output, Vec<OverviewEntry>)>) {
        for (output, windows) in &current {
            let known = self
                .output_windows
                .iter()
                .any(|(seen, seen_windows)| seen == output && seen_windows == windows);
            if known {
                continue;
            }
            for instance in &self.instances {
                send_output_windows(instance, output, windows);
            }
        }
        self.output_windows = current;
    }

    /// Publish what each display's colour pipeline can do and is doing.
    ///
    /// One list and one diff for two events, because it is one answer: HDR and
    /// the night light are the same stages on the same CRTC — see
    /// [`crate::hdr`] — and they are reported in the same breath the backend
    /// commits them in.
    fn broadcast_output_hdr(&mut self, current: Vec<(Output, crate::hdr::Status)>) {
        for (output, status) in &current {
            let known = self
                .output_hdr
                .iter()
                .any(|(seen, seen_status)| seen == output && seen_status == status);
            if known {
                continue;
            }
            for instance in &self.instances {
                send_output_hdr(instance, output, status);
                send_output_night_light(instance, output, status);
            }
        }
        self.output_hdr = current;
    }

    /// Publish what each display can be driven at, and which of those it is
    /// being driven at now.
    fn broadcast_output_modes(&mut self, current: Vec<(Output, Vec<DisplayMode>)>) {
        for (output, modes) in &current {
            let known = self
                .output_modes
                .iter()
                .any(|(seen, seen_modes)| seen == output && seen_modes == modes);
            if known {
                continue;
            }
            for instance in &self.instances {
                send_output_modes(instance, output, modes);
            }
        }
        self.output_modes = current;
    }

    /// Publish how each display's picture is turned.
    ///
    /// `current` lists only the displays this compositor turns itself, so one
    /// it does not — and one that has gone away — simply stops being named.
    fn broadcast_output_transform(&mut self, current: Vec<(Output, Transform)>) {
        for (output, transform) in &current {
            let known = self
                .output_transform
                .iter()
                .any(|(seen, seen_transform)| seen == output && seen_transform == transform);
            if known {
                continue;
            }
            for instance in &self.instances {
                send_output_transform(instance, output, *transform);
            }
        }
        self.output_transform = current;
    }

    /// Bring a newly bound shell up to date, since the broadcasts above only
    /// carry changes.
    ///
    /// Returns whether anything actually reached it: a client with no
    /// `wl_output` bound yet can be told nothing per-display.
    fn send_current(&self, shell: &LxbShellV1) -> bool {
        if shell.version() < PER_OUTPUT_SINCE {
            shell.foreground(self.foreground.clone());
            return true;
        }
        let mut sent = false;
        for (output, title) in &self.output_foreground {
            sent |= send_output_foreground(shell, output, title);
        }
        for (output, app_id) in &self.output_app_id {
            sent |= send_output_app_id(shell, output, app_id);
        }
        for (output, windows) in &self.output_windows {
            sent |= send_output_windows(shell, output, windows);
        }
        for (output, status) in &self.output_hdr {
            sent |= send_output_hdr(shell, output, status);
            sent |= send_output_night_light(shell, output, status);
        }
        for (output, modes) in &self.output_modes {
            sent |= send_output_modes(shell, output, modes);
        }
        for (output, transform) in &self.output_transform {
            sent |= send_output_transform(shell, output, *transform);
        }
        sent
    }

    /// Retry the full state for shells that had no outputs when they bound,
    /// until one of the sends lands.
    fn catch_up_new_shells(&mut self) {
        if self.awaiting_outputs.is_empty() {
            return;
        }
        let pending = std::mem::take(&mut self.awaiting_outputs);
        self.awaiting_outputs = pending
            .into_iter()
            .filter(|shell| shell.is_alive() && !self.send_current(shell))
            .collect();
    }
}

/// Send one display's foreground title, resolving the `wl_output` belonging to
/// the receiving client — an `Output` may have a different resource per client,
/// or none at all if that client never bound it.
///
/// Returns whether it reached the client, which is how a shell that has not
/// bound its outputs yet is told apart from one that is up to date.
fn send_output_foreground(shell: &LxbShellV1, output: &Output, title: &str) -> bool {
    if shell.version() < PER_OUTPUT_SINCE {
        return false;
    }
    let Some(client) = shell.client() else {
        return false;
    };
    let mut sent = false;
    for wl_output in output.client_outputs(&client) {
        shell.output_foreground(&wl_output, title.to_string());
        sent = true;
    }
    sent
}

/// Send one display's foreground application identity, resolved through the
/// receiving client's own `wl_output` for the same reason the title is.
fn send_output_app_id(shell: &LxbShellV1, output: &Output, app_id: &str) -> bool {
    if shell.version() < POINTER_SINCE {
        return false;
    }
    let Some(client) = shell.client() else {
        return false;
    };
    let mut sent = false;
    for wl_output in output.client_outputs(&client) {
        shell.output_app_id(&wl_output, app_id.to_string());
        sent = true;
    }
    sent
}

/// Send one display's HDR capability and state, resolved through the receiving
/// client's own `wl_output` for the same reason the title is.
fn send_output_hdr(shell: &LxbShellV1, output: &Output, status: &crate::hdr::Status) -> bool {
    if shell.version() < HDR_SINCE {
        return false;
    }
    let Some(client) = shell.client() else {
        return false;
    };
    let mut sent = false;
    for wl_output in output.client_outputs(&client) {
        shell.output_hdr(
            &wl_output,
            status.supported as u32,
            status.enabled as u32,
            status.max_luminance as u32,
        );
        // Which of the settings do something here, for shells new enough to
        // draw the difference between a control that is off and one that has
        // nothing to act on.
        if shell.version() >= HDR_CONTROLS_SINCE {
            let mut controls = lxb_shell_v1::HdrControl::empty();
            controls.set(lxb_shell_v1::HdrControl::Gamut, status.gamut);
            shell.output_hdr_controls(&wl_output, controls);
        }
        sent = true;
    }
    sent
}

/// Send one display's night light capability and state, out of the same status
/// the HDR event above is built from.
///
/// A separate event rather than two more arguments on that one, because the two
/// are separate answers: a laptop panel that will never do HDR can be warmed,
/// and a shell that read one for the other would leave the filter off exactly
/// the displays it is most wanted on. They travel together only because they
/// are committed together.
fn send_output_night_light(
    shell: &LxbShellV1,
    output: &Output,
    status: &crate::hdr::Status,
) -> bool {
    if shell.version() < NIGHT_LIGHT_SINCE {
        return false;
    }
    let Some(client) = shell.client() else {
        return false;
    };
    let mut sent = false;
    for wl_output in output.client_outputs(&client) {
        shell.output_night_light(&wl_output, status.night_light as u32, status.warming as u32);
        sent = true;
    }
    sent
}

/// Send one display's whole mode list, ending with the done event that makes
/// the batch replace whatever the shell knew before.
///
/// The done event goes out even when the list is empty, because "this display
/// offers nothing to choose between" is an answer a shell has to be able to
/// draw — and it is the answer on every nested session.
fn send_output_modes(shell: &LxbShellV1, output: &Output, modes: &[DisplayMode]) -> bool {
    if shell.version() < MODES_SINCE {
        return false;
    }
    let Some(client) = shell.client() else {
        return false;
    };
    let mut sent = false;
    for wl_output in output.client_outputs(&client) {
        for mode in modes {
            let mut flags = lxb_shell_v1::ModeFlag::empty();
            flags.set(lxb_shell_v1::ModeFlag::Current, mode.current);
            flags.set(lxb_shell_v1::ModeFlag::Preferred, mode.preferred);
            shell.output_mode(&wl_output, mode.width, mode.height, mode.refresh, flags);
        }
        shell.output_modes_done(&wl_output);
        sent = true;
    }
    sent
}

/// Send how one display's picture is turned, resolved through the receiving
/// client's own `wl_output` for the same reason the title is.
fn send_output_transform(shell: &LxbShellV1, output: &Output, transform: Transform) -> bool {
    if shell.version() < TRANSFORM_SINCE {
        return false;
    }
    let Some(client) = shell.client() else {
        return false;
    };
    let mut sent = false;
    for wl_output in output.client_outputs(&client) {
        shell.output_transform(&wl_output, wire_transform(transform));
        sent = true;
    }
    sent
}

/// One of the eight orientations as the protocol counts them, which is how
/// `wl_output` counts them.
fn wire_transform(transform: Transform) -> lxb_shell_v1::Transform {
    match transform {
        Transform::Normal => lxb_shell_v1::Transform::Normal,
        Transform::_90 => lxb_shell_v1::Transform::_90,
        Transform::_180 => lxb_shell_v1::Transform::_180,
        Transform::_270 => lxb_shell_v1::Transform::_270,
        Transform::Flipped => lxb_shell_v1::Transform::Flipped,
        Transform::Flipped90 => lxb_shell_v1::Transform::Flipped90,
        Transform::Flipped180 => lxb_shell_v1::Transform::Flipped180,
        Transform::Flipped270 => lxb_shell_v1::Transform::Flipped270,
    }
}

/// The same, read back off the wire. `None` for a value that is not one of the
/// eight, which a shell built against a later version of this protocol could
/// send and which must not be turned into a guess.
fn output_transform_of(transform: lxb_shell_v1::Transform) -> Option<Transform> {
    Some(match transform {
        lxb_shell_v1::Transform::Normal => Transform::Normal,
        lxb_shell_v1::Transform::_90 => Transform::_90,
        lxb_shell_v1::Transform::_180 => Transform::_180,
        lxb_shell_v1::Transform::_270 => Transform::_270,
        lxb_shell_v1::Transform::Flipped => Transform::Flipped,
        lxb_shell_v1::Transform::Flipped90 => Transform::Flipped90,
        lxb_shell_v1::Transform::Flipped180 => Transform::Flipped180,
        lxb_shell_v1::Transform::Flipped270 => Transform::Flipped270,
        _ => return None,
    })
}

/// Send one display's whole window list, ending with the done event that
/// makes the batch replace whatever the shell knew before.
fn send_output_windows(shell: &LxbShellV1, output: &Output, windows: &[OverviewEntry]) -> bool {
    if shell.version() < OVERVIEW_SINCE {
        return false;
    }
    let Some(client) = shell.client() else {
        return false;
    };
    let mut sent = false;
    for wl_output in output.client_outputs(&client) {
        for entry in windows {
            shell.output_window(
                &wl_output,
                entry.id,
                entry.title.clone(),
                entry.width,
                entry.height,
            );
            // Right behind the window it belongs to, so a shell can attach it
            // without matching anything up. Nothing is sent for a client that
            // named itself nothing: an empty name would be one more thing for
            // the shell to special-case, and its absence says the same.
            if shell.version() >= WINDOW_APP_ID_SINCE && !entry.app_id.is_empty() {
                shell.output_window_app_id(&wl_output, entry.id, entry.app_id.clone());
            }
        }
        shell.output_windows_done(&wl_output);
        sent = true;
    }
    sent
}

impl LxbState {
    /// Tell the shell the user asked for its overlay.
    pub fn open_guide(&mut self) {
        if !self.lxb.shell_control.has_shell() {
            tracing::debug!("guide binding pressed but no shell has bound lxb_shell_v1");
            return;
        }
        self.lxb.shell_control.send_guide();
    }

    /// Tell the shell the user asked for its keyboard.
    ///
    /// Routed here rather than left to the shell for the same reason as the
    /// guide: the key is pressed while an application holds the keyboard, and
    /// a keyboard that could only be summoned by a client already receiving
    /// keys would never be needed.
    pub fn open_keyboard(&mut self) {
        if !self.lxb.shell_control.wants_keyboard() {
            tracing::debug!("keyboard binding pressed but no shell is listening for it");
            return;
        }
        self.lxb.shell_control.send_keyboard();
    }

    /// Ask the shell to photograph the display the user is on.
    ///
    /// The compositor could take the picture without asking anybody — it has
    /// the pixels — and deliberately does not: where a screenshot goes is a
    /// question about the user's home directory, in the language their account
    /// was made in, and that is the shell's half of the session. So the
    /// binding is forwarded and the path comes back.
    pub fn screenshot_focused_output(&mut self) {
        if !self.lxb.shell_control.wants_screenshot() {
            tracing::debug!("screenshot binding pressed but no shell is listening for it");
            return;
        }
        // Where the user is, in order of how well each answer knows. Keyboard
        // focus first, because this *is* a key: whatever has the keys is on
        // the display the hand that pressed it is looking at — an application
        // holding them fullscreen, or the shell's own overlay. Then the
        // display the shell says it is being driven on, which is the answer
        // that survives a session where nothing has taken focus at all.
        let output = self
            .keyboard_focus_output()
            .or_else(|| self.shell_launch_output())
            .or_else(|| {
                self.lxb
                    .outputs
                    .output_at(&self.lxb.space, self.lxb.pointer_location)
            })
            .or_else(|| self.lxb.space.outputs().next().cloned());
        let Some(output) = output else {
            tracing::debug!("screenshot binding pressed with no display to photograph");
            return;
        };
        if !self.lxb.shell_control.send_screenshot(&output) {
            // The shell is new enough to be asked, but has not bound this
            // display's wl_output — it has only just started, and its outputs
            // have not arrived yet.
            tracing::debug!(
                display = %output.name(),
                "screenshot binding pressed on a display no shell has bound"
            );
        }
    }

    /// Photograph one display into `path`, and answer with where it went.
    ///
    /// `None` for every way this can fail, on the same terms as
    /// [`Self::capture_window_to`]: the shell's answer to all of them is the
    /// same, and which one it was is in the log here.
    pub fn capture_output_to(&mut self, output: &Output, path: &str) -> Option<String> {
        let path = std::path::Path::new(path);
        // Absolute, and into a directory that already exists — the same terms
        // a window capture is written on, for the same reason.
        if !path.is_absolute() {
            tracing::warn!(?path, "refusing to write a capture to a relative path");
            return None;
        }
        if !path.parent().is_some_and(|parent| parent.is_dir()) {
            tracing::warn!(
                ?path,
                "refusing to write a capture into a missing directory"
            );
            return None;
        }
        // The display has to still be one of ours: an output resource outlives
        // the connector by however long it takes the client to hear about it.
        if !self.lxb.space.outputs().any(|known| known == output) {
            tracing::debug!(display = %output.name(), "capture of a display that is gone");
            return None;
        }

        let shot = match self.backend.capture_output(&self.lxb, output) {
            Ok(shot) => shot,
            Err(err) => {
                tracing::warn!(?err, display = %output.name(), "could not photograph the display");
                return None;
            }
        };
        if let Err(err) = crate::capture::write_png(&shot, path) {
            tracing::warn!(?err, ?path, "could not write the capture out");
            return None;
        }
        tracing::info!(
            display = %output.name(),
            ?path,
            width = shot.width,
            height = shot.height,
            "photographed a display"
        );

        // Only now, and only because it worked: the flash says a picture was
        // taken, and it is started after the pixels have been read back so it
        // cannot be in the picture it is answering for.
        self.lxb.flashes.begin(output, std::time::Instant::now());
        self.queue_redraw();

        Some(path.to_string_lossy().into_owned())
    }

    /// Run an application without ever showing it, or stop doing so.
    ///
    /// Everything that makes a window *noticed* asks
    /// [`crate::state::Lxb::out_of_sight`] first, so this only has to record
    /// the answer and then make the screen agree with it: the windows that
    /// have just become invisible may be holding the keyboard, and the shell
    /// has been told they are on a display they are about to leave.
    pub fn keep_out_of_sight(&mut self, app_id: &str, hidden: bool) {
        // Folded the same way the window's own name will be when it is asked
        // about, which is the only reason the two ever meet. Naming nothing is
        // refused here rather than stored: an empty name in the set would hide
        // every window whose client never set an app_id, which on an X11-heavy
        // session is a great many of them.
        let Some(name) = crate::state::folded_app_id(app_id) else {
            tracing::debug!("the shell asked to hide an application with no name");
            return;
        };

        let changed = if hidden {
            self.lxb.unseen.insert(name.clone())
        } else {
            self.lxb.unseen.remove(&name)
        };
        if !changed {
            return;
        }
        tracing::info!(app_id = %name, hidden, "the shell changed what may be seen");

        // A window that has just been hidden cannot keep the keyboard: the
        // user would be typing into something they cannot see. Asking for the
        // topmost one again settles both directions — it skips what is now
        // hidden, and it picks up what has just been revealed.
        self.focus_topmost_window();
        // And the shell's own picture of what is running has to change with
        // it, or the guide goes on offering to close a window nobody can see.
        self.refresh_foreground();
        self.queue_redraw();
    }

    /// Publish the foreground application's title, if it changed — both for
    /// the session as a whole and for each display.
    ///
    /// The *topmost* window rather than the focused one: while the overlay is
    /// up the shell itself holds focus, and the application it is offering to
    /// close must not read as having disappeared.
    pub fn refresh_foreground(&mut self) {
        if !self.lxb.shell_control.has_shell() {
            return;
        }

        let title = self
            .topmost_application(None)
            .map(|window| window_title(&window))
            .unwrap_or_default();
        self.lxb.shell_control.broadcast_foreground(title);

        let outputs: Vec<Output> = self.lxb.space.outputs().cloned().collect();
        let per_output = outputs
            .iter()
            .map(|output| {
                let title = self
                    .topmost_application(Some(output))
                    .map(|window| window_title(&window))
                    .unwrap_or_default();
                (output.clone(), title)
            })
            .collect();
        self.lxb
            .shell_control
            .broadcast_output_foreground(per_output);

        // What that same window *is*, rather than what it currently says it
        // is. The shell files per-application settings under this.
        let per_output_app_id = outputs
            .iter()
            .map(|output| {
                let app_id = self
                    .topmost_application(Some(output))
                    .map(|window| window_app_id(&window))
                    .unwrap_or_default();
                (output.clone(), app_id)
            })
            .collect();
        self.lxb
            .shell_control
            .broadcast_output_app_id(per_output_app_id);

        // The overview's window lists ride the same refresh: they are diffed
        // per display, so an unchanged desktop sends nothing.
        let per_output_windows = outputs
            .into_iter()
            .map(|output| {
                let windows = crate::render::overview_windows(&self.lxb, &output)
                    .into_iter()
                    .map(|window| {
                        let size = self
                            .lxb
                            .space
                            .element_geometry(&window)
                            .map(|geometry| geometry.size)
                            .unwrap_or_default();
                        OverviewEntry {
                            id: crate::overview::window_id(&window),
                            title: window_title(&window),
                            app_id: window_app_id(&window),
                            width: size.w.max(0) as u32,
                            height: size.h.max(0) as u32,
                        }
                    })
                    .collect();
                (output, windows)
            })
            .collect();
        self.lxb
            .shell_control
            .broadcast_output_windows(per_output_windows);

        // What each display can do in HDR rides along too, for the same
        // reason: it is diffed, so a session where nobody touches it never
        // sends a second event.
        self.refresh_hdr();

        // Those broadcasts only carry changes, so a shell whose outputs were
        // not ready when it bound would otherwise wait for the desktop to
        // change before learning what is on it.
        self.lxb.shell_control.catch_up_new_shells();
    }

    /// Publish each display's HDR capability and state, if either changed.
    ///
    /// Called both from the refresh above and straight from the backend the
    /// moment a commit lands, because the answer to "did that work" has to
    /// reach the Settings column the user is looking at rather than wait for
    /// the next time a window moves.
    pub fn refresh_hdr(&mut self) {
        if !self.lxb.shell_control.has_shell() {
            return;
        }
        let statuses = self
            .lxb
            .space
            .outputs()
            .map(|output| (output.clone(), self.lxb.hdr.status(output)))
            .collect();
        self.lxb.shell_control.broadcast_output_hdr(statuses);
    }

    /// Publish what each display can be driven at, if that or the mode in use
    /// changed.
    ///
    /// Called when a display arrives or leaves and after a mode is set, rather
    /// than from the refresh that rides on window changes: a connector's list
    /// is fixed for as long as the cable is in, and rebuilding a few dozen
    /// modes per display every time a window moved would be work done for a
    /// diff that never fires.
    pub fn refresh_modes(&mut self) {
        if !self.lxb.shell_control.has_shell() {
            return;
        }
        let outputs: Vec<Output> = self.lxb.space.outputs().cloned().collect();
        let modes = outputs
            .into_iter()
            .map(|output| {
                let modes = self.output_modes(&output);
                (output, modes)
            })
            .collect();
        self.lxb.shell_control.broadcast_output_modes(modes);
    }

    /// Publish how each display's picture is turned, if any of them changed.
    ///
    /// Called on the same occasions the mode lists are — a display arriving or
    /// leaving, and a turn having been made — and for the same reason: an
    /// orientation changes only when somebody asks for it, so there is nothing
    /// for the refresh that rides on window changes to find.
    ///
    /// Displays whose picture this compositor does not turn are left out
    /// entirely rather than reported as unturned, which is what tells the
    /// shell's page apart from a display that is simply the right way up.
    pub fn refresh_transforms(&mut self) {
        if !self.lxb.shell_control.has_shell() {
            return;
        }
        let outputs: Vec<Output> = self.lxb.space.outputs().cloned().collect();
        let transforms = outputs
            .into_iter()
            .filter_map(|output| {
                let transform = self.output_transform(&output)?;
                Some((output, transform))
            })
            .collect();
        self.lxb
            .shell_control
            .broadcast_output_transform(transforms);
    }

    /// The window an overview id names, if it is still mapped.
    fn window_by_overview_id(&self, id: u32) -> Option<Window> {
        self.lxb
            .space
            .elements()
            .find(|window| crate::overview::window_id(window) == id)
            .cloned()
    }

    /// Ask the topmost application window to close itself, across the session
    /// or on one display.
    pub fn close_foreground_window(&mut self, output: Option<&Output>) {
        let Some(window) = self.topmost_application(output) else {
            tracing::debug!(
                display = output.map(|o| o.name()).unwrap_or_default(),
                "close requested with no application window on screen"
            );
            return;
        };
        self.request_window_close(&window);
    }

    /// End the application behind one window, without asking its permission.
    ///
    /// The shell's Close is a console's power-off for one application, not a
    /// file menu's Quit: it has to work on something that has stopped
    /// answering, which is exactly when `send_close` does nothing. What it
    /// must not do is end one *process* and call that the application —
    /// [`crate::teardown`] has the measurements, but in short, a supervised
    /// helper is replaced within seconds and the window comes back.
    ///
    /// So the process behind the window only starts the answer. An X11 window
    /// is the interesting case even for that: its Wayland client is Xwayland
    /// itself, and ending *that* would take the whole X session with it, so
    /// the pid is read from the window instead.
    pub fn kill_window(&mut self, window: &Window) {
        let pid = self.window_pid(window);
        let title = window_title(window);
        let app_id = window_app_id(window);
        let boundary = teardown::Boundary {
            shell: self.lxb.session_shell_pid,
            compositor: Some(std::process::id() as i32),
        };

        match teardown::ending(&app_id, pid, &teardown::Processes::read(), boundary) {
            teardown::Ending::Signal(doomed) => {
                tracing::info!(
                    %title,
                    %app_id,
                    processes = doomed.len(),
                    "ending application"
                );
                teardown::signal(&doomed, libc::SIGTERM);
                self.kill_survivors_after_grace(doomed, title);
            }
            teardown::Ending::Run(command) => {
                tracing::info!(%title, %app_id, command, "ending application by its own command");
                self.lxb.spawn(command);
            }
            teardown::Ending::Ask(fallback) => {
                tracing::info!(%title, %app_id, "asking application to close");
                self.request_window_close(window);
                self.run_if_window_survives_grace(crate::overview::window_id(window), fallback);
            }
            teardown::Ending::Unreachable => {
                // A client with no process here — forwarded from another
                // machine, or already gone. Asking is still better than doing
                // nothing.
                tracing::warn!(%title, %app_id, "no process to end; asking it to close instead");
                self.request_window_close(window);
            }
        }
    }

    /// Move a window to another display and leave it belonging there.
    ///
    /// One call does all of it: `tile_window_on_output` records the display on
    /// the window — which is what pins an application to a screen — and then
    /// sizes and places it across that screen's usable area, exactly as a
    /// window mapped there in the first place would have been.
    ///
    /// It is raised on arrival, because a window moved onto a display and left
    /// underneath whatever was already there has not visibly gone anywhere.
    /// Keyboard focus is not touched: the request comes from an overlay that is
    /// holding the keys on the display the user is standing on, and moving an
    /// application is not asking to follow it.
    pub fn move_window_to_output(&mut self, window: &Window, output: &Output) {
        if self
            .lxb
            .outputs
            .window_display(&self.lxb.space, window)
            .as_ref()
            == Some(output)
        {
            tracing::debug!(display = %output.name(), "window asked to move to the display it is on");
            return;
        }
        self.lxb
            .outputs
            .tile_window_on_output(&mut self.lxb.space, window, output);
        self.raise_window(window, false);
        tracing::info!(
            title = %window_title(window),
            display = %output.name(),
            "moved a window to another display"
        );
        // Both displays' window lists just changed, and the shell is drawing a
        // card deck out of them on the screen the user is looking at.
        self.refresh_foreground();
        self.queue_redraw();
    }

    /// Photograph one window into `path`, and answer with where it went.
    ///
    /// `None` is every way this can fail — the window closed, the path is not
    /// one the compositor will write to, the renderer could not read the
    /// window back, the file could not be created — because the shell's answer
    /// to all four is the same, and the difference is in the log here.
    pub fn capture_window_to(&mut self, id: u32, path: &str) -> Option<String> {
        let window = self.window_by_overview_id(id)?;
        let path = std::path::Path::new(path);
        // Absolute, and into a directory that already exists. The shell chose
        // this path; the compositor is not going to create a tree somewhere
        // from a string, nor resolve one against a working directory that
        // belongs to whoever started the session.
        if !path.is_absolute() {
            tracing::warn!(?path, "refusing to write a capture to a relative path");
            return None;
        }
        if !path.parent().is_some_and(|parent| parent.is_dir()) {
            tracing::warn!(
                ?path,
                "refusing to write a capture into a missing directory"
            );
            return None;
        }

        // The display it is on, for its scale: a picture of a window on a
        // doubled screen has twice the pixels, which is what was on screen.
        let scale = self
            .lxb
            .outputs
            .window_display(&self.lxb.space, &window)
            .map(|output| output.current_scale().fractional_scale())
            .unwrap_or(1.0);

        let shot = match self.backend.capture_window(&window, scale) {
            Ok(shot) => shot,
            Err(err) => {
                tracing::warn!(?err, title = %window_title(&window), "could not capture the window");
                return None;
            }
        };
        if let Err(err) = crate::capture::write_png(&shot, path) {
            tracing::warn!(?err, ?path, "could not write the capture out");
            return None;
        }
        tracing::info!(
            title = %window_title(&window),
            ?path,
            width = shot.width,
            height = shot.height,
            "captured a window"
        );
        Some(path.to_string_lossy().into_owned())
    }

    /// The process behind a window, however the window got here.
    fn window_pid(&self, window: &Window) -> Option<i32> {
        if let Some(surface) = window.x11_surface() {
            surface
                .pid()
                .or_else(|| surface.get_client_pid().ok())
                .map(|pid| pid as i32)
        } else {
            window
                .wl_surface()
                .and_then(|surface| surface.client())
                .and_then(|client| client.get_credentials(&self.lxb.display_handle).ok())
                .map(|credentials| credentials.pid)
        }
    }

    /// Kill whatever outlived the request to end it.
    ///
    /// This is what keeps Close a Close: an application is asked first, so it
    /// can write out what it was holding, but the asking has a deadline and
    /// nothing gets to sit behind a dismissed overlay arguing about it.
    fn kill_survivors_after_grace(&mut self, doomed: Vec<teardown::Doomed>, title: String) {
        let timer = Timer::from_duration(teardown::GRACE);
        let now = doomed.clone();
        let insert = self.lxb.loop_handle.insert_source(timer, move |_, _, _| {
            let killed = teardown::signal(&doomed, libc::SIGKILL);
            if killed > 0 {
                tracing::info!(%title, processes = killed, "killed what did not exit");
            }
            TimeoutAction::Drop
        });
        if let Err(err) = insert {
            // No timer means no deadline, and an application that has been
            // asked to end has to actually end. The grace is a courtesy; the
            // ending is not, so it happens now instead of never.
            tracing::warn!(?err, "cannot time the grace; ending it now");
            teardown::signal(&now, libc::SIGKILL);
        }
    }

    /// Run a command if a window is still on screen when the grace is up.
    ///
    /// The escape hatch for an application that is asked politely because that
    /// is what works for it, but might not answer: Waydroid's per-app windows
    /// close on request, and a Waydroid that stops answering is still ended by
    /// stopping its session.
    fn run_if_window_survives_grace(&mut self, id: u32, command: &'static str) {
        let timer = Timer::from_duration(teardown::GRACE);
        let insert =
            self.lxb
                .loop_handle
                .insert_source(timer, move |_, _, state: &mut LxbState| {
                    if state.window_by_overview_id(id).is_some() {
                        tracing::info!(id, command, "window ignored the close; ending it outright");
                        state.lxb.spawn(command);
                    }
                    TimeoutAction::Drop
                });
        if let Err(err) = insert {
            // Nothing left to check the window with, and the same rule
            // applies: Close ends things. This is heavier than it needs to be
            // for a window that was about to close on its own, and it only
            // happens when the event loop itself is refusing work.
            tracing::warn!(?err, "cannot check the window closed; ending it now");
            self.lxb.spawn(command);
        }
    }

    /// Topmost window that takes keyboard focus, across the session or on one
    /// display.
    fn topmost_application(&self, output: Option<&Output>) -> Option<Window> {
        self.lxb
            .space
            .elements()
            .rev()
            .find(|window| {
                window_accepts_keyboard_focus(window)
                    && !self.lxb.out_of_sight(window)
                    && match output {
                        Some(output) => self.primary_output(window).as_ref() == Some(output),
                        None => true,
                    }
            })
            .cloned()
    }

    /// The display a window is mostly on.
    ///
    /// `Space::elements_for_output` answers "overlaps at all", which lets a
    /// window that spills over an edge claim two displays — and then the guide
    /// on the second one offers to close an application the user is barely
    /// looking at. Largest overlap gives every window exactly one display,
    /// which is how a bar-per-display shell presents them.
    fn primary_output(&self, window: &Window) -> Option<Output> {
        let geometry = self.lxb.space.element_geometry(window)?;
        self.lxb
            .space
            .outputs()
            .filter_map(|output| {
                let area = self.lxb.space.output_geometry(output)?;
                let overlap = area.intersection(geometry)?;
                // i64: a pair of 4K displays already overflows i32 here.
                Some((
                    i64::from(overlap.size.w) * i64::from(overlap.size.h),
                    output,
                ))
            })
            .max_by_key(|(covered, _)| *covered)
            .map(|(_, output)| output.clone())
    }

    /// The display the shell last named, if it is still connected.
    ///
    /// Validated rather than trusted: a display can be unplugged between the
    /// shell naming it and an application actually starting.
    pub fn shell_launch_output(&self) -> Option<Output> {
        let wanted = self.lxb.shell_control.launch_output()?;
        self.lxb
            .space
            .outputs()
            .find(|output| *output == wanted)
            .cloned()
    }
}

/// A window's human-readable title, falling back to something the shell can
/// still put in a menu when the client set none.
fn window_title(window: &Window) -> String {
    if let Some(toplevel) = window.toplevel() {
        let title = with_states(toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().unwrap().title.clone())
        });
        if let Some(title) = title.filter(|title| !title.trim().is_empty()) {
            return title;
        }
    } else if let Some(surface) = window.x11_surface() {
        let title = surface.title();
        if !title.trim().is_empty() {
            return title;
        }
        let class = surface.class();
        if !class.trim().is_empty() {
            return class;
        }
    }
    // The event doubles as "something is running", so it must not be empty
    // just because a client never set a title.
    "Application".to_string()
}

/// What a window *is*, as opposed to what it is currently showing.
///
/// The `app_id` a toplevel sets, or an X11 window's class, both of which are
/// meant to be the same string every time that application runs — which is the
/// whole reason the shell asks for it rather than keying settings on a title
/// that changes with the open document.
///
/// Empty when the client set neither. A setting cannot be filed under nothing,
/// and the shell treats it as an application it is not allowed to remember
/// anything about, which is better than everything nameless sharing one entry.
pub(crate) fn window_app_id(window: &Window) -> String {
    if let Some(toplevel) = window.toplevel() {
        let app_id = with_states(toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().unwrap().app_id.clone())
        });
        return app_id
            .filter(|app_id| !app_id.trim().is_empty())
            .unwrap_or_default();
    }
    if let Some(surface) = window.x11_surface() {
        let class = surface.class();
        if !class.trim().is_empty() {
            return class;
        }
    }
    String::new()
}

impl GlobalDispatch<LxbShellV1, ()> for LxbState {
    fn bind(
        state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<LxbShellV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let shell = data_init.init(resource, ());
        // A shell that binds late still needs to know what is on screen, and
        // the broadcasts only carry changes. This early it will usually have
        // no `wl_output` bound yet, so it is queued for another try.
        if !state.lxb.shell_control.send_current(&shell) {
            state.lxb.shell_control.awaiting_outputs.push(shell.clone());
        }
        state.lxb.shell_control.instances.push(shell);
        tracing::info!("session shell bound lxb_shell_v1");

        // The first instance turns foreground tracking on; publish the current
        // window straight away rather than waiting for the next change.
        state.refresh_foreground();
        // And what every display can be driven at, and how each one's picture
        // is turned, which are otherwise only published when a display is
        // plugged in — on a session that started with its displays already
        // there, that is never.
        state.refresh_modes();
        state.refresh_transforms();
    }
}

impl Dispatch<LxbShellV1, ()> for LxbState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &LxbShellV1,
        request: lxb_shell_v1::Request,
        _data: &(),
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            lxb_shell_v1::Request::CloseForeground => state.close_foreground_window(None),
            lxb_shell_v1::Request::CloseOutputForeground { output } => {
                match Output::from_resource(&output) {
                    Some(output) => state.close_foreground_window(Some(&output)),
                    // The display went away between the shell drawing its menu
                    // and the user choosing from it.
                    None => tracing::debug!("close requested for a display that is gone"),
                }
            }
            lxb_shell_v1::Request::SetLaunchOutput { output } => {
                let output = Output::from_resource(&output);
                tracing::debug!(
                    display = output.as_ref().map(|o| o.name()).unwrap_or_default(),
                    "shell chose the display to launch applications on"
                );
                state.lxb.shell_control.launch_output = output;
            }
            lxb_shell_v1::Request::SetOutputOverview { output, enabled } => {
                match Output::from_resource(&output) {
                    Some(output) => {
                        tracing::debug!(
                            display = %output.name(),
                            enabled,
                            "shell toggled the window overview"
                        );
                        state
                            .lxb
                            .overview
                            .set(&output, enabled != 0, std::time::Instant::now());
                    }
                    None => tracing::debug!("overview toggled on a display that is gone"),
                }
            }
            lxb_shell_v1::Request::SetOverviewSelection { output, index } => {
                if let Some(output) = Output::from_resource(&output) {
                    state.lxb.overview.set_selection(&output, index as usize);
                }
            }
            lxb_shell_v1::Request::ActivateWindow { id } => {
                match state.window_by_overview_id(id) {
                    Some(window) => {
                        state.raise_window(&window, true);
                        state.set_window_keyboard_focus(&window);
                    }
                    // It closed between the shell drawing the card and the
                    // user choosing it.
                    None => tracing::debug!(id, "activation of a window that is gone"),
                }
            }
            lxb_shell_v1::Request::ActivateWindowFrom {
                id,
                x,
                y,
                width,
                height,
            } => {
                match state.window_by_overview_id(id) {
                    Some(window) => {
                        // The flight belongs to the display the window is on,
                        // which is the display the shell drew the tile on: an
                        // application is pinned to the display it opened on,
                        // so that is where it comes back.
                        let output = state
                            .lxb
                            .space
                            .outputs_for_element(&window)
                            .into_iter()
                            .next();
                        if let Some(output) = output {
                            state.lxb.restores.begin(
                                id,
                                &output,
                                lxb_protocol::overview::Rect {
                                    x: x as f64,
                                    y: y as f64,
                                    w: width as f64,
                                    h: height as f64,
                                },
                                std::time::Instant::now(),
                            );
                        }
                        state.raise_window(&window, true);
                        state.set_window_keyboard_focus(&window);
                        state.queue_redraw();
                    }
                    None => tracing::debug!(id, "restore of a window that is gone"),
                }
            }
            lxb_shell_v1::Request::KillWindow { id } => match state.window_by_overview_id(id) {
                Some(window) => state.kill_window(&window),
                None => tracing::debug!(id, "kill of a window that is gone"),
            },
            lxb_shell_v1::Request::MoveWindowToOutput { id, output } => {
                match (
                    state.window_by_overview_id(id),
                    Output::from_resource(&output),
                ) {
                    (Some(window), Some(output)) => state.move_window_to_output(&window, &output),
                    // Either end of the move can be gone by the time the user
                    // chooses it: the window closed, or the display was
                    // unplugged while its menu row was on screen.
                    (None, _) => tracing::debug!(id, "move of a window that is gone"),
                    (_, None) => tracing::debug!(id, "move to a display that is gone"),
                }
            }
            lxb_shell_v1::Request::CaptureWindow { id, path } => {
                // Answered on the object that asked, rather than broadcast: it
                // is the reply to one request, and a second shell listening did
                // not ask for a screenshot.
                let written = state.capture_window_to(id, &path).unwrap_or_default();
                if resource.version() >= WINDOW_MOVE_AND_CAPTURE_SINCE {
                    resource.window_captured(id, written);
                }
            }
            lxb_shell_v1::Request::CaptureOutput { output, path } => {
                // Answered on the object that asked, as a window capture is,
                // and answered even when the display has gone: a request that
                // got no reply is one the shell waits on forever.
                let written = match Output::from_resource(&output) {
                    Some(display) => state.capture_output_to(&display, &path).unwrap_or_default(),
                    None => {
                        tracing::debug!("capture of a display that is gone");
                        String::new()
                    }
                };
                resource.output_captured(&output, written);
            }
            lxb_shell_v1::Request::MovePointer { dx, dy } => {
                state.shell_move_pointer((dx, dy).into());
            }
            lxb_shell_v1::Request::ScrollPointer { dx, dy } => {
                state.shell_scroll_pointer(dx, dy);
            }
            lxb_shell_v1::Request::PointerButton {
                button,
                state: down,
            } => {
                // The enum is wl_pointer's own, so anything that is not
                // "pressed" is a release — including a value from a shell that
                // has learned a third one this compositor has not.
                let pressed = down
                    .into_result()
                    .is_ok_and(|down| down == lxb_shell_v1::ButtonState::Pressed);
                state.shell_pointer_button(button, pressed);
            }
            lxb_shell_v1::Request::SetOutputHdr {
                output,
                enabled,
                sdr_brightness,
                srgb_intensity,
                peak_brightness,
            } => {
                match Output::from_resource(&output) {
                    Some(output) => {
                        let settings = crate::hdr::Settings {
                            enabled: enabled != 0,
                            // Clamped rather than rejected. A shell asking for
                            // a white level of zero has a bug, and a display
                            // left dark is a worse way to report it than one
                            // showing the nearest thing that makes sense.
                            sdr_brightness: sdr_brightness.clamp(1, u16::MAX as u32) as u16,
                            srgb_intensity: srgb_intensity.min(100) as u8,
                            peak_brightness: (peak_brightness != 0)
                                .then(|| peak_brightness.min(u16::MAX as u32) as u16),
                        };
                        if state.lxb.hdr.request(&output, settings) {
                            // Only once it has actually changed something, so a
                            // shell that re-sends what a display is already in
                            // does not rewrite the file every time it connects.
                            crate::remembered::remember(&output.name(), |entry| {
                                entry.hdr = Some(settings.enabled);
                                entry.hdr_sdr_brightness = Some(settings.sdr_brightness);
                                entry.hdr_srgb_intensity = Some(settings.srgb_intensity);
                                // `None` is "whatever the display says about
                                // itself", and has to be written down as the
                                // absence of the key rather than as a zero.
                                entry.hdr_peak_brightness = settings.peak_brightness;
                            });
                            // The commit happens on this display's next frame,
                            // which on an idle session is up to a retrace away
                            // and on a covered one may never come.
                            state.queue_redraw();
                        }
                    }
                    None => tracing::debug!("HDR requested for a display that is gone"),
                }
            }
            lxb_shell_v1::Request::SetOutputNightLight {
                output,
                enabled,
                temperature,
            } => {
                match Output::from_resource(&output) {
                    Some(output) => {
                        let night = crate::hdr::NightLight {
                            enabled: enabled != 0,
                            // Clamped rather than rejected, for the reason the
                            // white level above is: a shell asking for a
                            // temperature this cannot encode has a bug, and a
                            // display left at some unrelated colour is a worse
                            // way to report it than the nearest one that means
                            // something.
                            temperature: temperature.clamp(
                                crate::hdr::WARMEST_KELVIN as u32,
                                crate::hdr::NEUTRAL_KELVIN as u32,
                            ) as u16,
                        };
                        if state.lxb.hdr.request_night_light(&output, night) {
                            // What the shell sends here is the schedule already
                            // worked out — whether the light should be burning
                            // now, not whether it is switched on — because the
                            // compositor owns no clock. Remembering the answer
                            // is still right: what the next compositor wants is
                            // the picture that is on the screen at the moment it
                            // takes over, and the shell corrects a stale one
                            // within the second at the cost of a gamma ramp,
                            // which is not a modeset and so is not a black
                            // screen.
                            crate::remembered::remember(&output.name(), |entry| {
                                entry.night_light = Some(night.enabled);
                                entry.night_light_temperature = Some(night.temperature);
                            });
                            // As the HDR request: the ramp is committed on
                            // this display's next frame, which on an idle
                            // session is a retrace away.
                            state.queue_redraw();
                        }
                    }
                    None => {
                        tracing::debug!("a night light was requested for a display that is gone")
                    }
                }
            }
            lxb_shell_v1::Request::SetOutputMode {
                output,
                width,
                height,
                refresh,
            } => {
                match Output::from_resource(&output) {
                    Some(output) => {
                        // Nothing is clamped into range here the way the HDR
                        // values are: a mode is not a number with a sensible
                        // neighbour, and a size the connector does not list is
                        // refused rather than rounded to one it does.
                        let want = crate::config::ModeRequest {
                            width: width.min(i32::MAX as u32) as i32,
                            height: height.min(i32::MAX as u32) as i32,
                            // 0 is "the fastest of that size", which is what a
                            // shell asking only about the resolution sends.
                            refresh: (refresh != 0).then(|| refresh.min(i32::MAX as u32) as i32),
                        };
                        if state.set_output_mode(&output, want) {
                            crate::remembered::remember(&output.name(), |entry| {
                                entry.mode = Some(want.as_config_string());
                            });
                        }
                    }
                    None => tracing::debug!("a mode was requested for a display that is gone"),
                }
            }
            lxb_shell_v1::Request::SetOutputTransform { output, transform } => {
                let turn = transform.into_result().ok().and_then(output_transform_of);
                match (Output::from_resource(&output), turn) {
                    (Some(output), Some(turn)) => {
                        if state.set_output_transform(&output, turn) {
                            crate::remembered::remember(&output.name(), |entry| {
                                entry.transform =
                                    Some(crate::outputs::transform_name(turn).to_owned());
                            });
                            // What the display is now drawing, for the page
                            // that asked — and for the page on every other
                            // display, which lists this one too.
                            state.refresh_transforms();
                        }
                    }
                    (None, _) => tracing::debug!("a display that is gone was asked to turn"),
                    // Not one of the eight: a shell built against a later
                    // version of this protocol than the compositor is.
                    (_, None) => tracing::debug!(
                        "a display was asked for an orientation this compositor does not have"
                    ),
                }
            }
            lxb_shell_v1::Request::AskToShare { id, app_id } => {
                tracing::info!(id, %app_id, "an application is asking to see a display");
                if !state
                    .lxb
                    .shell_control
                    .send_share_request(resource, id, &app_id)
                {
                    // Nobody to ask, so nobody said yes. Refused here and now
                    // rather than left for a timeout somewhere else, because a
                    // session with no shell is not one that is about to grow
                    // one mid-question.
                    tracing::info!(id, "refusing: no shell to put the question to");
                    resource.share_answered(id, None);
                }
            }
            lxb_shell_v1::Request::AnswerShare { id, output } => {
                let chosen = output.as_ref().and_then(Output::from_resource);
                tracing::info!(
                    id,
                    display = chosen.as_ref().map(|o| o.name()).unwrap_or_default(),
                    "the shell answered a share request"
                );
                state
                    .lxb
                    .shell_control
                    .send_share_answer(id, chosen.as_ref());
            }
            lxb_shell_v1::Request::KeepOutOfSight { app_id, hidden } => {
                state.keep_out_of_sight(&app_id, hidden == 1)
            }
            lxb_shell_v1::Request::HidePointer => state.pointer_put_down(),
            lxb_shell_v1::Request::KeyboardKey { key, state: down } => {
                // As above: anything that is not "pressed" is a release.
                let pressed = down
                    .into_result()
                    .is_ok_and(|down| down == lxb_shell_v1::KeyState::Pressed);
                state.shell_keyboard_key(key, pressed);
            }
            lxb_shell_v1::Request::Quit => {
                tracing::info!("session shell requested shutdown");
                state.lxb.running = false;
                state.lxb.loop_signal.stop();
            }
            lxb_shell_v1::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &LxbShellV1, _data: &()) {
        state
            .lxb
            .shell_control
            .instances
            .retain(|instance| instance != resource);
        // Anything this client was waiting on is nobody's business now.
        state.lxb.shell_control.forget_shares(resource);
        // And anything anybody else was waiting on has lost the client that
        // could have said yes to it. An unanswered question is a no.
        if state.lxb.shell_control.instances.len() < 2 {
            state.lxb.shell_control.refuse_all_shares();
        }

        // With no shell left there is nobody to close the overview, and a
        // desktop whose windows are all shrunk into cards is unusable. A
        // shell that crashes or is restarted must not leave one behind.
        if !state.lxb.shell_control.has_shell() {
            state.lxb.overview.close_all(std::time::Instant::now());
        }
    }
}
