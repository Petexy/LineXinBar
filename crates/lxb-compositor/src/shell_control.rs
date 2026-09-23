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
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::Transform;
use smithay::wayland::compositor::with_states;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

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
    /// The process that drew it, where the compositor knows one. `None` for a
    /// client whose socket carried no credentials and for an X11 window whose
    /// owner set no `_NET_WM_PID`.
    ///
    /// Diffed with the rest for the same reason the name is, and for one more:
    /// a window whose pid arrives late — an X11 window is adopted before it has
    /// said anything about itself — is a window list that changed, and a shell
    /// told once and never again would be left matching on the moment it
    /// appeared. See `lxb_shell_v1.output_window_pid`.
    pub pid: Option<u32>,
    pub width: u32,
    pub height: u32,
}

/// One window the compositor is keeping off the screen, as the shell is told
/// about it: what it is, and what it is showing.
///
/// No size and no display, unlike [`OverviewEntry`] — a hidden window is not
/// on a display as far as anything else in the session is concerned, and
/// nothing is going to draw a card of it. What it is for is the one question
/// the shell has to answer about it: is this the application getting on with
/// what it was asked, or is it stopping to ask something?
///
/// Compared as a whole, so a client renaming or retitling a hidden window is a
/// list that changed. Both do happen while a window is hidden: an X11 client
/// announces its class after it maps, and Valve's client retitles the same
/// window as it walks a wizard.
#[derive(Debug, Clone, PartialEq)]
pub struct UnseenEntry {
    pub id: u32,
    pub app_id: String,
    pub title: String,
}

/// One floating window as the shell is told about it: the id, and the whole
/// rectangle the user sees — the surround included, since that is what a menu is
/// grown out of and what a mark is drawn around.
///
/// Compared rather than merely carried: this is the list that changes every
/// frame while somebody drags a window, and the diff on it is what keeps a
/// session where nobody does from sending anything at all.
#[derive(Debug, Clone, PartialEq)]
pub struct FloatingEntry {
    pub id: u32,
    pub rect: lxb_protocol::overview::Rect,
}

/// One launch the shell has in flight, and how to recognise the window it
/// turns into. See `lxb_shell_v1.place_launch`.
///
/// It exists because "where do new windows go" and "which launch is this
/// window" are the same question asked once per launch, and the compositor
/// used to have room for only one answer. Two games started on two displays
/// whose windows arrive in the other order were two windows placed by whichever
/// launch was named last — both on one screen, and neither on the screen it was
/// pressed on.
///
/// Matched by process first and by name second, because the two are known in
/// different cases and neither covers both. The shell forks an ordinary
/// application itself and has its pid; a game handed to a store client is
/// started by the client and the shell has nothing but the class it will
/// arrive under. A record with neither is still worth filing — its output is
/// read by nothing else, but it costs nothing and keeps the shell's side of
/// this one shape rather than two.
#[derive(Debug, Clone)]
struct LaunchRecord {
    /// The shell's own number for the press. Opaque here: all it does is tell
    /// one record from another and let the shell take this one back.
    launch: u32,
    /// The display it was started from, which is the whole answer this carries.
    output: Output,
    /// The process the shell started, if it started one itself.
    pid: Option<u32>,
    /// What its window will call itself, if the shell can say.
    app_id: Option<String>,
    /// When it was filed, for [`UNTIL_A_LAUNCH_IS_STALE`].
    filed: std::time::Instant,
}

/// File one launch, replacing any record already under that number, and drop
/// what nobody came back for.
///
/// Swept here rather than on a timer: the list is only ever read while a window
/// is mapping and only ever grows when a press is made, so the moment a press
/// is made is the cheapest place to drop a record the shell never took back.
/// See [`UNTIL_A_LAUNCH_IS_STALE`].
fn file_launch(launches: &mut Vec<LaunchRecord>, record: LaunchRecord) {
    let now = record.filed;
    launches.retain(|kept| kept.launch != record.launch && !kept.stale(now));
    if launches.len() >= MOST_LAUNCH_RECORDS {
        launches.remove(0);
    }
    launches.push(record);
}

/// Which filed launch a window belongs to, if any.
///
/// The newest match wins. Two records can name one program — the same game
/// pressed twice on two screens, an application started again before the first
/// arrived — and the later press is the one whose window has had the least time
/// to turn up. It is the same rule the shell's own single answer used, applied
/// where it now belongs.
///
/// `descends` is how the process chain is walked, handed in rather than read
/// here so this rule can be checked against a tree of one's own; the session
/// passes [`descends_from`], which walks `/proc`.
fn launch_for<'a>(
    launches: &'a [LaunchRecord],
    now: std::time::Instant,
    pid: Option<u32>,
    app_id: &str,
    descends: impl Fn(u32, u32) -> bool,
) -> Option<&'a LaunchRecord> {
    launches
        .iter()
        .rev()
        .filter(|record| !record.stale(now))
        .find(|record| {
            record.claims_app_id(app_id)
                || pid.is_some_and(|pid| record.pid.is_some_and(|started| descends(pid, started)))
        })
}

/// The display the user is driving, out of the two things a shell may have
/// named.
///
/// The split this rule is the whole of: `set_launch_output` is where an
/// application *opens*, which a shell holds on the display a game was started
/// from for as long as the game takes to arrive, and `set_driven_output` is
/// where the person *is*. Read as one field they contradict each other for the
/// whole of a loading time, and every question about the person — the keyboard
/// coming back, the X screen's active window, which screen a screenshot
/// binding photographs — moved to a display nobody was looking at.
///
/// The fallback is what a shell older than version 41 says, which is one value
/// for both. Reading it here is exactly the behaviour this compositor had
/// before the two came apart, so such a shell is neither better nor worse off.
fn the_display_being_driven<'a>(
    driven: Option<&'a Output>,
    launch: Option<&'a Output>,
) -> Option<&'a Output> {
    driven.or(launch)
}

impl LaunchRecord {
    fn stale(&self, now: std::time::Instant) -> bool {
        now.duration_since(self.filed) > UNTIL_A_LAUNCH_IS_STALE
    }

    /// Whether this record is about a window that calls itself `app_id`.
    ///
    /// Exact, and case-folded because an X11 class is whatever the client
    /// wrote and the same program spells its own name both ways across
    /// toolkits. An empty name on either side matches nothing: a window that
    /// set no class must not be claimed by a record that named none.
    fn claims_app_id(&self, app_id: &str) -> bool {
        let Some(wanted) = self.app_id.as_deref() else {
            return false;
        };
        !app_id.trim().is_empty() && wanted.eq_ignore_ascii_case(app_id.trim())
    }
}

/// Tracks every shell bound to the protocol, plus the state they were last
/// told about.
#[derive(Debug)]
pub struct ShellControlState {
    #[allow(dead_code)]
    global: GlobalId,
    /// Every shell bound to the interface. Shells only: the portal binds this
    /// interface too and is kept in [`Self::portals`] instead, because almost
    /// nothing here is addressed to it. Every broadcast below walks this list,
    /// and a portal in it would be sent the title of each window, the list of
    /// them, and the pid behind each one — none of which it has any use for,
    /// and all of which is a running account of what the user is doing.
    instances: Vec<LxbShellV1>,
    /// Every portal bound to the interface, kept apart from the shells for the
    /// reason above and for one more: what a bound object may *ask* for is
    /// decided by which of these two lists it is in. See [`Role`].
    portals: Vec<LxbShellV1>,
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
    /// The windows being kept out of sight, session-wide rather than per
    /// display for the reason [`UnseenEntry`] carries no display.
    unseen_windows: Vec<UnseenEntry>,
    /// Last floating window list broadcast per display, likewise — and diffed
    /// harder than most, because these rectangles move: a window being dragged
    /// is a new list every frame, and one standing still is the same list
    /// forever.
    output_pip: Vec<(Output, Vec<FloatingEntry>)>,
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
    /// Last place broadcast per display, for the displays this compositor
    /// arranges. Diffed like the rest, and one display moving moves at least
    /// one other — they trade — so a change here is normally two events.
    output_place: Vec<(Output, usize)>,
    /// Display new applications open on by default, from `set_launch_output`.
    ///
    /// The fallback rather than the whole answer since version 41: a window
    /// that matches one of [`Self::launches`] goes where that record says, and
    /// this is what places everything else.
    launch_output: Option<Output>,
    /// Display the user is driving, from `set_driven_output`.
    ///
    /// Read for the questions that are about where the person is — which
    /// display gives the keyboard back, which display's window is the X
    /// screen's active one, which display a screenshot binding photographs —
    /// and never for placing a window.
    ///
    /// `None` on a shell older than version 41, which said one thing for both;
    /// see [`LxbState::shell_driven_output`], which falls back to the launch
    /// output so that such a shell keeps the behaviour it always had.
    driven_output: Option<Output>,
    /// One record per launch the shell has in flight, from `place_launch`.
    ///
    /// A list rather than one value because a session can be loading two things
    /// on two displays at once, and a single value is a window opening on
    /// whichever launch happened to be named last. See [`LaunchRecord`].
    launches: Vec<LaunchRecord>,
    /// Display a press on an application was last reported on, so that clicking
    /// about inside a game does not wake the shell once per click.
    ///
    /// Forgotten whenever the shell names a launch display, because that is the
    /// shell moving of its own accord — with a shoulder button, or by coming
    /// back to a window from the guide — and a press back on the display it
    /// moved away from has to be reported again.
    pressed_output: Option<Output>,
    /// Whether the next key pressed on a keyboard is worth telling the shell
    /// about.
    ///
    /// Armed when a shell binds — one that has just started has been told
    /// nothing — and again whenever a shell says the controller is back in the
    /// user's hands. Spent by the event being sent, because what the shell
    /// wants out of it is that the hands have moved: a message somebody types
    /// into a game would otherwise wake it once per letter to say what the
    /// first letter already said.
    typing_is_news: bool,
    /// Last answer broadcast per display to "is an application in front of
    /// this one": whether there is a window there at all, or whether the shell
    /// is the whole of what is on the screen. Diffed like the rest — it changes
    /// when something is opened or closed and at no other time.
    output_in_use: Vec<(Output, bool)>,
    /// Last answer broadcast per display to "is anything moving here". Diffed
    /// like the rest, but unlike the rest it is a reading of the clock rather
    /// than of the desktop's shape, so it is taken once a pass of the loop.
    output_drawing: Vec<(Output, bool)>,
    /// Display the pointer was last reported over, and when it was reported,
    /// so a pointer being moved about sends an event every couple of seconds
    /// rather than one per motion. See [`Self::pointer_moved`].
    pointer_output: Option<(Output, std::time::Instant)>,
    /// Questions in flight: who asked, and what number they gave it.
    ///
    /// One list rather than one per client, because an answer names only the
    /// question — the shell has no idea who is behind it, and should not: what
    /// it is answering is "may this application see a screen", not "reply to
    /// that client".
    asked: Vec<Question>,
    /// File questions in flight, on exactly the terms [`Self::asked`] holds
    /// share questions — and a second list rather than a second kind of
    /// [`Question`] in the first, because the two are answered by different
    /// requests and an answer that found the wrong sort would be a screen
    /// shared because somebody picked a file.
    picking: Vec<Question>,
    /// The files the shell has named for a question it has not yet ended.
    ///
    /// Held only between one `chose_file` and the `answer_pick` that follows
    /// it, because that is the whole of what a wayland request can say: one
    /// path each, in order, and then "that is all of them". Keyed by the
    /// question, so two questions being answered at once cannot pour into one
    /// another.
    chosen: Vec<(u32, String)>,
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

/// First version that says where each display stands in the arrangement, and
/// can be asked to move one. Below it a shell has no way to know which screen
/// the compositor puts first, and the displays are laid out in the order they
/// were plugged in.
const PLACE_SINCE: u32 = 21;

/// First version that forwards the volume keys. Below it they are keys like
/// any other and go to whatever holds the keyboard, which is to say that under
/// a game they do nothing at all.
const VOLUME_SINCE: u32 = 22;

/// First version that says which display a press landed on. Below it a shell
/// learns of a press only where its own surfaces are in front, so clicking the
/// application on the second screen left it driving the first one.
const PRESSED_SINCE: u32 = 23;

/// First version that says a key was pressed on a keyboard, and that can be
/// told the controller is back. Below it a shell learns of typing only where it
/// holds the keys itself, so a user typing into a game keeps being offered a
/// keyboard they are already sitting at.
const TYPED_SINCE: u32 = 24;

/// First version that can be asked to draw applications larger than life, for
/// every display at once. Below it every window is the size of the display it is
/// on, and a shell's Application scaling page still remembers what it was set to
/// — the file is read by whichever compositor comes next — but nothing is sent.
///
/// See [`PER_DISPLAY_APP_SCALE_SINCE`], which is the same setting said about one
/// screen; this one is what the screens nobody has named draw at.
const APP_SCALE_SINCE: u32 = 25;

/// First version that can be asked to fade every display to black, and that
/// says when the black is on screen. Below it a shell that turns the machine
/// off has nothing to put over the session first, and the picture stops
/// wherever the kernel happened to catch it.
const CURTAIN_SINCE: u32 = 26;

/// First version that can rest one display behind black while another one is
/// being used, and that reports the two facts a shell cannot see for itself:
/// which display has an application in front of it, and which has anything
/// still painting. Below it the OLED protection page still remembers what it
/// was set to — the file is read by whichever compositor comes next — but no
/// screen is ever rested.
const RESTING_SINCE: u32 = 27;

/// First version that can be told an application is playing something and must
/// not be stopped while it is out of sight. Below it the sleeper is the whole
/// rule — an application nobody can see is stopped, whatever it was in the
/// middle of — which is what every session did before this and is still what a
/// session with no shell does.
const MEDIA_SINCE: u32 = 28;

/// First version that can be told what to do with a browser's
/// picture-in-picture window. Below it such a window is an application window
/// like any other — maximized, listed, focusable — which is what every session
/// did before this and is still what a session with no shell does.
const PIP_SINCE: u32 = 29;

/// First version that can be asked for the floating window's own menu, and that
/// can answer with what the user chose. Below it the right button on such a
/// window is the client's, as it is on every other window.
const PIP_MENU_SINCE: u32 = 30;

/// First version that lists the floating windows to the shell, that can be told
/// which of them the user's own controls are on, and that will move one on a
/// controller's word. Below it a floating window is something only a pointer can
/// reach.
const PIP_PAD_SINCE: u32 = 31;

/// First version that will draw one surface of the shell's own in front of the
/// floating windows instead of behind them: the one its context menu is on, and
/// nothing else. Below it such a window covers every menu raised over it, which
/// on a session with no pointer is a window that cannot be made small again.
const MENU_SURFACE_SINCE: u32 = 32;

/// First version that will draw, into a buffer the shell hands over, what it is
/// compositing behind the shell's own surfaces.
///
/// The shell draws glass and glass shows what is behind it. What the shell
/// draws it can read back, and the wallpaper it can evaluate — but another
/// client's window it can do neither with, so below this a pane standing over a
/// game or over somebody's video reproduces the wallpaper instead. See
/// `lxb_shell_v1.ask_for_the_picture_behind`.
const PICTURE_BEHIND_SINCE: u32 = 33;

/// First version that will take the user's own word for whether one window
/// floats: a video told to fill the display it is in the corner of, and an
/// application told to go and sit in that corner instead. Below it the title is
/// the whole of the answer, and a video put in a corner can only be got out of
/// it by the browser that put it there.
const WHICH_WINDOWS_FLOAT_SINCE: u32 = 34;

/// First version that will put a *file* question to the shell: which file, or
/// which folder, or what to call a new one. Below it the session has no file
/// chooser of its own, and an application asking `xdg-desktop-portal` for one
/// is answered by whatever other backend is installed — or, on a machine with
/// none, by nothing at all.
const PICK_SINCE: u32 = 35;

/// First version that forwards the switch binding: one step of the walk the
/// user makes along the session's applications with the modifier held down,
/// and the modifier coming up at the end of it. Below it there is no deck to
/// walk — the pictures of the running applications are the shell's overlay —
/// so the binding rotates the compositor's own window stack instead, which is
/// all a session with no shell has ever had.
const SWITCH_SINCE: u32 = 36;

/// First version that carries the keyboard layout: the xkb layout and variant
/// the seat's keyboards are set to, and the event that says what they are.
/// Below it a session's keyboard is whatever the compositor's own config file
/// says, which is where this lived before there was a page for it.
///
/// Version 37 was the pointing-device settings — how fast the pointer travels,
/// how large the cursor is drawn, and how far and which way a wheel carries the
/// content. It has no constant of its own because nothing gates on it: a shell
/// that can send `set_pointer` at all is one that bound a version carrying it,
/// and there is no older behaviour to fall back to. The mouse's own settings
/// lived in this compositor's config file before there was a page for them, and
/// still do where no shell says otherwise.
const LAYOUT_SINCE: u32 = 38;

/// First version that says which process drew each listed window.
///
/// Gated, and it is one of the few events where an absent one is not the same
/// as an absent answer: below this version a shell has nothing but the moment a
/// window appeared to match it by, which is the guess this replaces rather than
/// a worse version of the same fact.
const WINDOW_PID_SINCE: u32 = 39;

/// Version 40 says what is being kept out of sight, and takes one window back
/// out of it: `unseen_window`, `unseen_windows_done` and
/// `let_this_window_be_seen`. A program the shell runs without showing still
/// stops and asks things, and a question nobody can see is a session that has
/// silently stopped.
const UNSEEN_WINDOWS_SINCE: u32 = 40;

/// First version that tells the driven display from the launch display, and
/// files a record per launch: `set_driven_output`, `place_launch` and
/// `forget_launch`.
///
/// Gated in both directions, and the fallback is exactly what this compositor
/// did before it. A shell below this version names one output for both
/// questions, so [`LxbState::shell_driven_output`] reads `launch_output` when
/// nothing has named a driven one, and a session with no launch records placed
/// falls back to `launch_output` for every window it maps — which is the whole
/// of the old policy, unchanged.
const LAUNCH_RECORDS_SINCE: u32 = 41;

/// The version that tells a shell whether the gamut conversion it is offering
/// is the exact one: the `gamut_exact` bit on `output_hdr_controls`.
///
/// Gated because the bit's absence has to mean "this compositor does not say"
/// rather than "the conversion is approximate". A shell below this version is
/// told only `gamut`, and reads it the way it always did — as the exact
/// conversion, which before this version was the only kind a display carrying
/// that bit could do.
const EXACT_GAMUT_SINCE: u32 = 42;

/// First version that can be asked how large applications draw themselves on
/// *one named display*: `set_output_application_scale`.
///
/// Gated by nothing, because there is nothing to gate — a request a shell
/// cannot send is one this compositor never hears. What the version number does
/// is let a shell tell whether asking per display will work: below it,
/// `set_application_scale` is the only way to say this, and it says it for
/// every screen at once.
///
/// Both are still served. The session-wide one is the floor under the
/// per-display one — see [`crate::outputs::OutputManager::app_scale_on`] — so a
/// shell that only knows the old request gets exactly what it always got.
const PER_DISPLAY_APP_SCALE_SINCE: u32 = 43;

/// First version that can be told how many pixels one application draws its
/// picture at: `set_application_resolution`.
///
/// Gated by nothing here, for the reason the version above it is gated by
/// nothing: a request a shell cannot send is one this compositor never hears.
/// What the number does is let a shell tell whether the setting can be carried
/// out at all — below it every application draws at the size of the display it
/// is on, and a shell's own menu still remembers what was chosen, because the
/// file is read by whichever compositor comes next.
const APP_RESOLUTION_SINCE: u32 = 44;

/// How long a launch record is kept without anything matching it.
///
/// The shell drops its own with `forget_launch` on every path out of a press,
/// and this is what stops a record outliving a shell that somehow did not —
/// a shell that crashed mid-launch, or one whose splash ended by a route
/// nobody remembered to wire up. Comfortably past every patience the shell
/// spends on a window, so it is never what ends a record that matters: the
/// longest of those is the minute a game handed to Valve's client is given,
/// plus the wake before it.
const UNTIL_A_LAUNCH_IS_STALE: std::time::Duration = std::time::Duration::from_secs(300);

/// How many launch records are kept at once.
///
/// One per display would do; this is loose enough that nothing legitimate ever
/// reaches it and tight enough that a shell filing records and never dropping
/// them cannot grow this without bound. The oldest goes when a new one arrives
/// past the cap.
const MOST_LAUNCH_RECORDS: usize = 32;

/// What is answered when no kind of file was in force — because the
/// application offered none, or because the user was looking at everything on
/// the disk rather than at one of the kinds. `lxb_shell_v1.answer_pick`'s own
/// number for it, quoted here so the two halves cannot disagree about which
/// index means "not one of them".
const NO_KIND: u32 = u32::MAX;

/// How long a display may go without anything painting on it before it counts
/// as still — the reading behind `lxb_shell_v1.output_drawing`.
///
/// Three seconds, chosen from both ends. Long enough that a film dropping
/// frames, a game between levels, or a page waiting on the network is still
/// something somebody is watching; short enough that a film somebody paused has
/// stopped being one well before the shell's own idle timer runs out, so a
/// screen that is going to rest does not sit lit for a further minute first.
const STILL: std::time::Duration = std::time::Duration::from_secs(3);

/// How often a pointer that goes on moving over the same display is reported.
///
/// The shell uses this to keep a display awake, so it has to be comfortably
/// shorter than the idle it is keeping the display away from — five seconds —
/// and is otherwise as long as it can be. Two seconds turns a mouse dragged
/// across a game for a minute into thirty events rather than several thousand.
const POINTER_REPEAT: std::time::Duration = std::time::Duration::from_secs(2);

/// The version advertised, and so the highest a shell can bind. Every request
/// below it is still served, so an older shell keeps working.
const CURRENT_VERSION: u32 = APP_RESOLUTION_SINCE;

/// Each constant above names the one feature that arrived in its version, and
/// the numbers only ever go up by one. Said here so that two branches each
/// claiming "the next version" cannot both be merged — the easy mistake, and
/// one that otherwise shows up as a shell silently not being sent an event.
const _: () = assert!(NIGHT_LIGHT_SINCE == OUT_OF_SIGHT_SINCE + 1);
const _: () = assert!(PLACE_SINCE == NIGHT_LIGHT_SINCE + 1);
const _: () = assert!(VOLUME_SINCE == PLACE_SINCE + 1);
const _: () = assert!(PRESSED_SINCE == VOLUME_SINCE + 1);
const _: () = assert!(TYPED_SINCE == PRESSED_SINCE + 1);
const _: () = assert!(APP_SCALE_SINCE == TYPED_SINCE + 1);
const _: () = assert!(CURTAIN_SINCE == APP_SCALE_SINCE + 1);
const _: () = assert!(RESTING_SINCE == CURTAIN_SINCE + 1);
const _: () = assert!(MEDIA_SINCE == RESTING_SINCE + 1);
const _: () = assert!(PIP_SINCE == MEDIA_SINCE + 1);
const _: () = assert!(PIP_MENU_SINCE == PIP_SINCE + 1);
const _: () = assert!(PIP_PAD_SINCE == PIP_MENU_SINCE + 1);
const _: () = assert!(MENU_SURFACE_SINCE == PIP_PAD_SINCE + 1);
const _: () = assert!(PICTURE_BEHIND_SINCE == MENU_SURFACE_SINCE + 1);
const _: () = assert!(WHICH_WINDOWS_FLOAT_SINCE == PICTURE_BEHIND_SINCE + 1);
const _: () = assert!(PICK_SINCE == WHICH_WINDOWS_FLOAT_SINCE + 1);
const _: () = assert!(SWITCH_SINCE == PICK_SINCE + 1);
const _: () = assert!(WINDOW_PID_SINCE == LAYOUT_SINCE + 1);
const _: () = assert!(UNSEEN_WINDOWS_SINCE == WINDOW_PID_SINCE + 1);
const _: () = assert!(LAUNCH_RECORDS_SINCE == UNSEEN_WINDOWS_SINCE + 1);
const _: () = assert!(EXACT_GAMUT_SINCE == LAUNCH_RECORDS_SINCE + 1);
const _: () = assert!(PER_DISPLAY_APP_SCALE_SINCE == EXACT_GAMUT_SINCE + 1);
const _: () = assert!(APP_RESOLUTION_SINCE == PER_DISPLAY_APP_SCALE_SINCE + 1);

/// What a client allowed onto this protocol is allowed to do with it.
///
/// Two programs of a LineXinBar session bind `lxb_shell_v1`, and they want very
/// different things from it. The shell drives the session and uses nearly every
/// request there is. The portal carries one question from an application
/// outside the session to the shell — may this be shared, which file — and
/// needs three of them. Handing both the whole interface would mean any flaw in
/// the portal, which is the process that talks to untrusted applications for a
/// living, costing the session its keyboard, its windows and its screen
/// contents. So the two are told apart at the bind and each is held to its own
/// half. See [`portal_may_ask`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The session shell: the whole interface.
    Shell,
    /// The desktop portal: `offer_kind`, `ask_to_pick_files` and
    /// `ask_to_share`, and nothing else.
    Portal,
}

/// The session's own processes, by the one thing about a process that another
/// process cannot counterfeit.
///
/// A pid, and specifically the pid of a child this compositor started itself
/// and has not yet reaped. The kernel stamps it on the socket at connect
/// (`SO_PEERCRED`), nothing on the far end can influence it, and it cannot be
/// taken by anything else while the child is alive — a process that has exited
/// but not been waited for still holds its number, so the number is only ever
/// released at the moment this compositor reaps it, which is the same moment it
/// forgets it here.
///
/// It used to be a name as well: any process whose executable was called
/// `lxb-desktop` or `lxb-portal` was let through. A name is not a credential.
/// This session deliberately runs programs it cannot vet — Valve's client and
/// every game it starts, every Flatpak out of the software hub, a browser — all
/// as the same user, and any one of them could copy itself to a file of that
/// name and connect. What it would then hold is the interface that hides any
/// window, closes anybody's, injects keys and pointer clicks, captures the
/// screen and ends the session.
#[derive(Default)]
struct Privileged {
    /// The session shell, from [`this_is_the_session_shell`].
    shell: Option<i32>,
    /// The desktop portal, from [`this_is_the_session_portal`].
    portal: Option<i32>,
    /// Program names an operator has explicitly told this session to trust,
    /// from `--insecure-trust-program`.
    ///
    /// Empty in every session that did not ask for it, which is every session
    /// that is not being worked on: this is the old spoofable rule, kept behind
    /// a flag nobody types by accident, because a shell or a portal run by hand
    /// beside the compositor has no pid the compositor could have learned —
    /// `lxb-portal --debug-pick` is how the file chooser is looked at. Trusting
    /// a name is trusting every process of this user that can write a file, so
    /// it is off unless somebody says otherwise. See [`trust_these_programs`].
    by_name: Vec<String>,
}

/// Who may drive this compositor, as the whole process knows it.
///
/// A static rather than a field because of where the question is asked from:
/// `can_view` decides whether a client may so much as see this global, and it
/// is handed the client and nothing else — no compositor state, no display.
static PRIVILEGED: std::sync::Mutex<Privileged> = std::sync::Mutex::new(Privileged {
    shell: None,
    portal: None,
    by_name: Vec::new(),
});

fn privileged() -> std::sync::MutexGuard<'static, Privileged> {
    match PRIVILEGED.lock() {
        Ok(privileged) => privileged,
        Err(held) => held.into_inner(),
    }
}

/// The compositor has started the session shell, and this is its pid.
pub fn this_is_the_session_shell(pid: i32) {
    tracing::info!(pid, "this process is the session shell and may drive it");
    privileged().shell = Some(pid);
}

/// The session shell has been reaped.
///
/// Called from the same step that waits for it, and that ordering is the whole
/// point: until the child is reaped its number is still its own, and after it is
/// reaped the next process to start could be given it. Forgetting it here in
/// between leaves no moment where something else wearing that number would be
/// taken for the shell.
pub fn the_session_shell_has_gone() {
    privileged().shell = None;
}

/// The compositor has started the session's desktop portal, and this is its pid.
pub fn this_is_the_session_portal(pid: i32) {
    tracing::info!(pid, "this process is the session portal");
    privileged().portal = Some(pid);
}

/// The portal has been reaped. On the terms [`the_session_shell_has_gone`] sets
/// out, and it matters more here: the session outlives its portal, so the
/// number really is handed out again.
pub fn the_session_portal_has_gone() {
    privileged().portal = None;
}

/// Trust these program names as the session shell as well, because an operator
/// asked for it on the command line.
///
/// Every name here is one any process of this user can wear, so this is only
/// ever what somebody developing the session typed. Said out loud in the log,
/// once, so a machine running with it cannot do so quietly.
pub fn trust_these_programs(names: &[String]) {
    // A name no process can have is not a name. `program_of` is the basename of
    // `/proc/<pid>/exe`, which is never empty, so an empty entry could only sit
    // in the trusted list looking as though it had been taken — on the one flag
    // in this compositor where what is and is not trusted has to be legible.
    if names.iter().any(|name| name.trim().is_empty()) {
        tracing::warn!("ignoring an empty --insecure-trust-program name: no process has one");
    }
    let names: Vec<String> = names
        .iter()
        .filter(|name| !name.trim().is_empty())
        .cloned()
        .collect();
    if names.is_empty() {
        return;
    }
    let names = &names[..];
    tracing::warn!(
        ?names,
        "trusting these program names on lxb_shell_v1: any process of this user \
         can take a name, so this session's control protocol is open to all of them"
    );
    privileged().by_name = names.to_vec();
}

/// Whether this client may bind `lxb_shell_v1` at all, and as what.
///
/// Answered from the credentials the kernel put on the socket when the client
/// connected — see [`crate::state::ClientState::for_peer`] — and never from
/// anything the client says about itself.
///
/// Refusing is safe in one direction only, so it is worth saying which. A
/// client wrongly refused loses this protocol, and for the shell that is the
/// whole session; a client wrongly allowed can hide windows and press keys. The
/// ways in are therefore all *positive* evidence, and a session that started no
/// shell of its own recognises nothing rather than everything.
pub fn role_of(client: &Client) -> Option<Role> {
    let who = client.get_data::<crate::state::ClientState>()?;
    role_for(who.pid, who.program.as_deref(), &privileged())
}

/// Whether this client may bind `lxb_shell_v1`, in either part.
pub fn may_drive_the_shell(client: &Client) -> bool {
    role_of(client).is_some()
}

/// The decision itself, on the facts it is made of.
fn role_for(pid: Option<i32>, program: Option<&str>, privileged: &Privileged) -> Option<Role> {
    if let Some(pid) = pid {
        if privileged.shell == Some(pid) {
            return Some(Role::Shell);
        }
        if privileged.portal == Some(pid) {
            return Some(Role::Portal);
        }
    }
    // Only ever reached in a session started with `--insecure-trust-program`.
    // A name that got this far is trusted as a shell, because what the flag is
    // for is standing in for one.
    let program = program?;
    privileged
        .by_name
        .iter()
        .any(|trusted| trusted == program)
        .then_some(Role::Shell)
}

/// Whether the portal may ask this.
///
/// Three requests, and each of them is the same act: putting a question from an
/// application outside the session in front of the user, and waiting to be told
/// what they said. `offer_kind` describes the kinds of file a chooser should
/// offer, `ask_to_pick_files` is the chooser itself, `ask_to_share` is the
/// screen-sharing consent. Nothing else on this interface is any of the
/// portal's business: the answers come back on `pick_chosen`, `pick_answered`
/// and `share_answered`, which are events, and the requests that *give* those
/// answers belong to the shell.
///
/// Written as a list of what is allowed rather than what is not, so a request
/// added to this protocol later is out of the portal's reach until somebody
/// decides otherwise.
fn portal_may_ask(request: &lxb_shell_v1::Request) -> bool {
    matches!(
        request,
        lxb_shell_v1::Request::OfferKind { .. }
            | lxb_shell_v1::Request::AskToPickFiles { .. }
            | lxb_shell_v1::Request::AskToShare { .. }
            | lxb_shell_v1::Request::Destroy
    )
}

impl ShellControlState {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<LxbShellV1, ()> + 'static,
    {
        Self {
            global: display.create_global::<D, LxbShellV1, _>(CURRENT_VERSION, ()),
            instances: Vec::new(),
            portals: Vec::new(),
            awaiting_outputs: Vec::new(),
            foreground: String::new(),
            output_foreground: Vec::new(),
            output_app_id: Vec::new(),
            output_windows: Vec::new(),
            unseen_windows: Vec::new(),
            output_pip: Vec::new(),
            output_hdr: Vec::new(),
            output_modes: Vec::new(),
            output_transform: Vec::new(),
            output_place: Vec::new(),
            output_in_use: Vec::new(),
            output_drawing: Vec::new(),
            pointer_output: None,
            launch_output: None,
            driven_output: None,
            launches: Vec::new(),
            pressed_output: None,
            typing_is_news: true,
            asked: Vec::new(),
            picking: Vec::new(),
            chosen: Vec::new(),
        }
    }

    /// Whether a shell is listening. Used only to explain an inert binding.
    pub fn has_shell(&self) -> bool {
        !self.instances.is_empty()
    }

    /// Whether this bound object is the portal's rather than the shell's.
    ///
    /// Which list it went into at the bind is the whole of the answer, and the
    /// bind put it there from the pid the kernel stamped on its connection.
    fn is_portal(&self, instance: &LxbShellV1) -> bool {
        self.portals.contains(instance)
    }

    pub fn launch_output(&self) -> Option<&Output> {
        self.launch_output.as_ref()
    }

    pub fn driven_output(&self) -> Option<&Output> {
        self.driven_output.as_ref()
    }

    /// File one launch, replacing any record already under that number.
    ///
    /// Pruned here rather than on a timer: the list is only ever read while a
    /// window is mapping and only ever grows when a press is made, so the
    /// moment a press is made is the cheapest place to drop what the shell
    /// never came back for. See [`UNTIL_A_LAUNCH_IS_STALE`].
    fn place_launch(&mut self, record: LaunchRecord) {
        file_launch(&mut self.launches, record);
    }

    fn forget_launch(&mut self, launch: u32) {
        self.launches.retain(|kept| kept.launch != launch);
    }

    /// Whether `client` is the session shell — the one that bound
    /// `lxb_shell_v1`.
    ///
    /// Asked when two clients want the same thing and only the shell may have
    /// it. Holding this protocol as a shell is what makes a client the shell,
    /// so it is also the only honest way to tell it apart from an application.
    /// The portal holds the same interface and is not the shell: it is in
    /// [`Self::portals`], which this does not look at.
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

    /// Whether any shell listening can draw the deck a walk along the
    /// applications is made on. A compositor with none rotates its own stack
    /// instead, so this decides between the two rather than explaining a key
    /// that did nothing.
    fn wants_switch(&self) -> bool {
        self.instances
            .iter()
            .any(|instance| instance.version() >= SWITCH_SINCE)
    }

    fn send_switch(&self, direction: lxb_shell_v1::SwitchDirection) {
        for instance in &self.instances {
            if instance.version() >= SWITCH_SINCE {
                instance.switch_window(direction);
            }
        }
    }

    fn send_switch_done(&self) {
        for instance in &self.instances {
            if instance.version() >= SWITCH_SINCE {
                instance.switch_done();
            }
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

    /// Whether any shell listening can be told about a volume key. Used only
    /// to explain a key that did nothing.
    fn wants_volume(&self) -> bool {
        self.instances
            .iter()
            .any(|instance| instance.version() >= VOLUME_SINCE)
    }

    fn send_volume(&self, change: lxb_shell_v1::VolumeChange) {
        for instance in &self.instances {
            if instance.version() >= VOLUME_SINCE {
                instance.volume(change);
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

    /// Tell every shell that a press landed on an application, and on which
    /// display, so that the display the user is driving follows their hand onto
    /// a screen the shell's own surfaces are not in front of.
    ///
    /// Nothing is sent for a press on the display this last reported — see
    /// [`Self::pressed_output`] — and the display is only remembered once it has
    /// actually been sent, so a press made before any shell had bound that
    /// `wl_output` is not recorded as delivered.
    pub(crate) fn send_output_pressed(&mut self, output: &Output) {
        if !press_is_news(self.pressed_output.as_ref(), output) {
            return;
        }
        let mut sent = false;
        for instance in &self.instances {
            if instance.version() < PRESSED_SINCE {
                continue;
            }
            let Some(client) = instance.client() else {
                continue;
            };
            for wl_output in output.client_outputs(&client) {
                instance.output_pressed(&wl_output);
                sent = true;
            }
        }
        if sent {
            tracing::debug!(display = %output.name(), "a press landed on an application here");
            self.pressed_output = Some(output.clone());
        }
    }

    /// Ask a shell to raise the floating window's menu over `rect`.
    ///
    /// `true` when some shell was actually told, which is what decides whether
    /// the press is the compositor's at all: a session with no shell new enough
    /// to draw the menu has no menu, and the right button on that window is
    /// better left to the browser that owns it than swallowed for nothing.
    ///
    /// Sent to every bound shell, like everything else here. Two shells drawing
    /// two menus is not a case this session has, and the alternative — picking
    /// one — would be this deciding which of them the user is looking at.
    pub(crate) fn send_pip_menu(
        &self,
        id: u32,
        output: &Output,
        rect: lxb_protocol::overview::Rect,
    ) -> bool {
        let mut sent = false;
        for instance in &self.instances {
            if instance.version() < PIP_MENU_SINCE {
                continue;
            }
            let Some(client) = instance.client() else {
                continue;
            };
            for wl_output in output.client_outputs(&client) {
                instance.pip_menu(
                    id,
                    &wl_output,
                    rect.x.round() as i32,
                    rect.y.round() as i32,
                    rect.w.round() as i32,
                    rect.h.round() as i32,
                );
                sent = true;
            }
        }
        if sent {
            tracing::debug!(id, display = %output.name(), "asked the shell for the floating window's menu");
        }
        sent
    }

    /// Tell every shell that a key went down on a keyboard, so that whatever it
    /// is offering a controller can be put away.
    ///
    /// Once, and then nothing until a shell says the controller has been picked
    /// back up — see [`Self::typing_is_news`]. Spent only once it has actually
    /// been sent, so a key pressed before any shell new enough had bound this
    /// interface is not recorded as delivered.
    pub(crate) fn send_typed(&mut self) {
        if !self.typing_is_news {
            return;
        }
        let mut sent = false;
        for instance in &self.instances {
            if instance.version() < TYPED_SINCE {
                continue;
            }
            instance.typed();
            sent = true;
        }
        if sent {
            tracing::debug!("a key was pressed on a keyboard; the shell is told");
            self.typing_is_news = false;
        }
    }

    /// Tell every shell that the black it asked for is on every display.
    ///
    /// This is the one event the session's own exit waits on: the shell runs
    /// the command that ends the machine when it arrives. Sent once per
    /// curtain — [`crate::curtain::Curtain::everything_is_black`] is what says
    /// so — because a shutdown asked for twice is one asked for once and once
    /// more into a session that is already going.
    pub(crate) fn send_screen_is_black(&self) {
        for instance in &self.instances {
            if instance.version() < CURTAIN_SINCE {
                continue;
            }
            instance.screen_is_black();
        }
    }

    /// Arm that event again: the user has picked the controller back up, or a
    /// shell has just bound and has been told nothing yet.
    fn typing_is_news_again(&mut self) {
        self.typing_is_news = true;
    }

    /// Put one client's question to everybody else bound to this interface,
    /// which in a running session is the shell.
    ///
    /// The shells and not the asker. A portal is never in this list at all, so
    /// a question cannot come back to the client that asked it — which would be
    /// a portal answering itself — and one portal cannot be handed another
    /// portal's question to answer. The asker is skipped as well, for the one
    /// case left: a shell that asks.
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

    /// Carry one kind of file through to the shell, ahead of the question it
    /// belongs to.
    ///
    /// Nothing is kept here. The order requests arrive in from one client is
    /// the order events go out in to another, so the kinds land in front of
    /// their own `pick_request` without this having to remember what a question
    /// was made of — see `lxb_shell_v1.offer_kind`, which is where that is
    /// written down and why.
    fn send_pick_kind(
        &mut self,
        asker: &LxbShellV1,
        id: u32,
        name: &str,
        pattern: &str,
        matching: lxb_shell_v1::Matching,
    ) {
        for instance in &self.instances {
            if instance == asker || instance.version() < PICK_SINCE {
                continue;
            }
            instance.pick_kind(id, name.to_string(), pattern.to_string(), matching);
        }
    }

    /// Put one client's file question to everybody else bound to this
    /// interface, which in a running session is the shell.
    ///
    /// Never back to the asker, for the reason [`Self::send_share_request`] is
    /// not: the portal binds this interface too, and a question that came back
    /// to the client that asked it would be a portal answering itself.
    #[allow(clippy::too_many_arguments)]
    fn send_pick_request(
        &mut self,
        asker: &LxbShellV1,
        id: u32,
        app_id: &str,
        purpose: lxb_shell_v1::Picking,
        title: &str,
        accept: &str,
        name: &str,
        at: &str,
    ) -> bool {
        let mut asked = false;
        for instance in &self.instances {
            if instance == asker || instance.version() < PICK_SINCE {
                continue;
            }
            instance.pick_request(
                id,
                app_id.to_string(),
                purpose,
                title.to_string(),
                accept.to_string(),
                name.to_string(),
                at.to_string(),
            );
            asked = true;
        }
        if asked {
            self.picking.push(Question {
                asker: asker.clone(),
                id,
            });
        }
        asked
    }

    /// Note one file the shell says the user chose, until the question it
    /// belongs to is ended.
    ///
    /// Capped, because this is a list one client can add to without ever
    /// finishing: a shell naming ten thousand files for a question it never
    /// answers would be a shell filling the compositor's memory. Past the cap
    /// the file is dropped and said so — the user gets the files they picked
    /// first, which is a truthful subset, rather than the session growing
    /// without bound.
    fn note_chosen_file(&mut self, id: u32, path: String) {
        /// How many files one unanswered question may name. A selection nobody
        /// makes by hand is past this long before it matters.
        const MOST: usize = 4096;
        if self.chosen.iter().filter(|(named, _)| *named == id).count() >= MOST {
            tracing::warn!(
                id,
                "too many files named for one question; dropping this one"
            );
            return;
        }
        self.chosen.push((id, path));
    }

    /// Tell whoever asked what the user chose, and end the question.
    ///
    /// The files go first and the answer last, which is the order the protocol
    /// promises: a client reads `pick_chosen` until `pick_answered` arrives,
    /// and what it has by then is the whole answer.
    fn send_pick_answer(&mut self, id: u32, kind: u32) {
        // The oldest outstanding question with this number, on exactly the
        // terms a share answer finds its own — and, as there, this cannot tell
        // a shell answering out of order from one answering in it, only keep it
        // from answering the same question twice.
        let Some(index) = self.picking.iter().position(|question| question.id == id) else {
            // Nothing to answer, so nothing to keep either: files named for a
            // question that was never asked would otherwise sit here for the
            // life of the session.
            self.chosen.retain(|(named, _)| *named != id);
            return;
        };
        let question = self.picking.remove(index);
        let mut files = Vec::new();
        self.chosen.retain(|(named, path)| {
            if *named == id {
                files.push(path.clone());
                return false;
            }
            true
        });
        for path in files {
            question.asker.pick_chosen(id, path);
        }
        question.asker.pick_answered(id, kind);
    }

    /// Forget the file questions of a client that has gone, and the files named
    /// for them. There is nobody left to tell.
    fn forget_picks(&mut self, gone: &LxbShellV1) {
        let dropped: Vec<u32> = self
            .picking
            .iter()
            .filter(|question| &question.asker == gone)
            .map(|question| question.id)
            .collect();
        self.picking.retain(|question| &question.asker != gone);
        self.chosen.retain(|(id, _)| !dropped.contains(id));
    }

    /// Refuse every file question outstanding: the shell that was going to
    /// answer has gone, and an application left waiting for a file it will
    /// never be given is worse than one told plainly that it has none.
    fn refuse_all_picks(&mut self) {
        for question in std::mem::take(&mut self.picking) {
            self.chosen.retain(|(id, _)| *id != question.id);
            question.asker.pick_answered(question.id, NO_KIND);
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

    /// Publish what is being kept off the screen, resending only when the list
    /// itself changes.
    ///
    /// One list rather than one per display, and one diff over the whole of it:
    /// nearly every session hides nothing at all, and for those this compares
    /// an empty list with an empty list once a pass and sends nothing.
    fn broadcast_unseen_windows(&mut self, current: Vec<UnseenEntry>) {
        if self.unseen_windows == current {
            return;
        }
        self.unseen_windows = current;
        for instance in &self.instances {
            send_unseen_windows(instance, &self.unseen_windows);
        }
    }

    /// Publish the floating windows on each display, so a shell with no pointer
    /// has something to point its own controls at.
    fn broadcast_output_pip(&mut self, current: Vec<(Output, Vec<FloatingEntry>)>) {
        for (output, windows) in &current {
            let known = self
                .output_pip
                .iter()
                .any(|(seen, seen_windows)| seen == output && seen_windows == windows);
            if known {
                continue;
            }
            for instance in &self.instances {
                send_output_pip(instance, output, windows);
            }
        }
        self.output_pip = current;
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

    /// Publish which displays have an application in front of them.
    ///
    /// Diffed like the rest, and it barely moves: the answer changes when
    /// something is opened and when it is closed, and never in between.
    fn broadcast_output_in_use(&mut self, current: Vec<(Output, bool)>) {
        for (output, in_use) in &current {
            let known = self
                .output_in_use
                .iter()
                .any(|(seen, seen_in_use)| seen == output && seen_in_use == in_use);
            if known {
                continue;
            }
            for instance in &self.instances {
                send_output_in_use(instance, output, *in_use);
            }
        }
        self.output_in_use = current;
    }

    /// Publish which displays still have something painting on them.
    ///
    /// The one broadcast here whose answer moves without the desktop changing
    /// shape — a film ends, somebody pauses it — so it is taken once a pass of
    /// the session's loop rather than when a window maps. The diff is what
    /// keeps that from being an event per pass.
    fn broadcast_output_drawing(&mut self, current: Vec<(Output, bool)>) {
        for (output, drawing) in &current {
            let known = self
                .output_drawing
                .iter()
                .any(|(seen, seen_drawing)| seen == output && seen_drawing == drawing);
            if known {
                continue;
            }
            for instance in &self.instances {
                send_output_drawing(instance, output, *drawing);
            }
        }
        self.output_drawing = current;
    }

    /// Say that the pointer is being moved over `output`, if that is news.
    ///
    /// News means one of two things: it has arrived on a different display from
    /// the one last reported, or it is still on the same one and has been
    /// moving for [`POINTER_REPEAT`] since anybody was told. Everything else is
    /// swallowed here — a pointer crossing a game at sixty motions a second is
    /// one event every two seconds, and a pointer standing still is none.
    ///
    /// What the shell does with it is time out a display it has been left
    /// alone with, so the repeat has to be shorter than that timeout and is
    /// otherwise as long as it can be. See `lxb_shell_v1.output_pointer`.
    pub fn pointer_moved(&mut self, output: &Output, now: std::time::Instant) {
        let news = match &self.pointer_output {
            Some((seen, told)) => {
                seen != output || now.saturating_duration_since(*told) >= POINTER_REPEAT
            }
            None => true,
        };
        if !news {
            return;
        }
        self.pointer_output = Some((output.clone(), now));
        for instance in &self.instances {
            send_output_pointer(instance, output);
        }
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

    /// Publish where each display stands in the arrangement.
    ///
    /// `current` lists only the displays this compositor arranges, and lists
    /// them in the order it arranges them, so one it does not — and one that
    /// has gone away — simply stops being named.
    fn broadcast_output_place(&mut self, current: Vec<(Output, usize)>) {
        for (output, place) in &current {
            let known = self
                .output_place
                .iter()
                .any(|(seen, seen_place)| seen == output && seen_place == place);
            if known {
                continue;
            }
            for instance in &self.instances {
                send_output_place(instance, output, *place);
            }
        }
        self.output_place = current;
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
        for (output, windows) in &self.output_pip {
            sent |= send_output_pip(shell, output, windows);
        }
        // Not per display, so it does not need a wl_output and cannot be the
        // thing that makes this a wasted call: `sent` is deliberately not
        // touched by it.
        send_unseen_windows(shell, &self.unseen_windows);
        for (output, status) in &self.output_hdr {
            sent |= send_output_hdr(shell, output, status);
            sent |= send_output_night_light(shell, output, status);
        }
        for (output, in_use) in &self.output_in_use {
            sent |= send_output_in_use(shell, output, *in_use);
        }
        for (output, drawing) in &self.output_drawing {
            sent |= send_output_drawing(shell, output, *drawing);
        }
        for (output, modes) in &self.output_modes {
            sent |= send_output_modes(shell, output, modes);
        }
        for (output, transform) in &self.output_transform {
            sent |= send_output_transform(shell, output, *transform);
        }
        for (output, place) in &self.output_place {
            sent |= send_output_place(shell, output, *place);
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

/// Whether a press on `output` is news to the shell, given the display the last
/// one was reported on.
///
/// The whole of the diff, so that clicking about inside a game does not wake the
/// shell once per click: a press moves the display being driven, and a press on
/// the display already being driven moves nothing.
fn press_is_news(last: Option<&Output>, output: &Output) -> bool {
    last != Some(output)
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
            // And, for a shell new enough to draw the difference, whether the
            // conversion that control makes is the exact one. Sent only where
            // it can be told apart from silence — see `EXACT_GAMUT_SINCE`.
            if shell.version() >= EXACT_GAMUT_SINCE {
                controls.set(lxb_shell_v1::HdrControl::GamutExact, status.gamut_exact);
            }
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

/// Send whether an application is in front of one display, resolved through
/// the receiving client's own `wl_output` for the reason the title is.
fn send_output_in_use(shell: &LxbShellV1, output: &Output, in_use: bool) -> bool {
    if shell.version() < RESTING_SINCE {
        return false;
    }
    let Some(client) = shell.client() else {
        return false;
    };
    let mut sent = false;
    for wl_output in output.client_outputs(&client) {
        shell.output_in_use(&wl_output, in_use as u32);
        sent = true;
    }
    sent
}

/// Send whether anything on one display is still painting.
///
/// A separate event from the one above rather than two arguments on it, for the
/// reason the night light is separate from HDR: they are separate answers. A
/// display with a game on it is drawing and a display with a paused film on it
/// is not, and a shell that read one for the other would rest the screen
/// somebody is playing on.
fn send_output_drawing(shell: &LxbShellV1, output: &Output, drawing: bool) -> bool {
    if shell.version() < RESTING_SINCE {
        return false;
    }
    let Some(client) = shell.client() else {
        return false;
    };
    let mut sent = false;
    for wl_output in output.client_outputs(&client) {
        shell.output_drawing(&wl_output, drawing as u32);
        sent = true;
    }
    sent
}

/// Say the pointer is over one display, resolved the same way.
fn send_output_pointer(shell: &LxbShellV1, output: &Output) -> bool {
    if shell.version() < RESTING_SINCE {
        return false;
    }
    let Some(client) = shell.client() else {
        return false;
    };
    let mut sent = false;
    for wl_output in output.client_outputs(&client) {
        shell.output_pointer(&wl_output);
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

/// Send where one display stands in the arrangement, resolved through the
/// receiving client's own `wl_output` for the same reason the title is.
fn send_output_place(shell: &LxbShellV1, output: &Output, place: usize) -> bool {
    if shell.version() < PLACE_SINCE {
        return false;
    }
    let Some(client) = shell.client() else {
        return false;
    };
    let mut sent = false;
    for wl_output in output.client_outputs(&client) {
        shell.output_place(&wl_output, place.min(u32::MAX as usize) as u32);
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

/// How large a floating window was asked to be, read off the wire. `None` for a
/// value this version has no meaning for — which a shell built against a later
/// one could send, and which must leave the size where it is rather than become
/// a guess.
fn pip_size_of(size: lxb_shell_v1::PipSize) -> Option<lxb_protocol::pip::Size> {
    use lxb_protocol::pip::Size;
    Some(match size {
        lxb_shell_v1::PipSize::Small => Size::Small,
        lxb_shell_v1::PipSize::Medium => Size::Medium,
        lxb_shell_v1::PipSize::Large => Size::Large,
        _ => return None,
    })
}

/// Which row of the floating window's menu was chosen, on the same terms.
fn pip_command_of(command: lxb_shell_v1::PipCommand) -> Option<crate::pip::MenuCommand> {
    use crate::pip::MenuCommand;
    Some(match command {
        lxb_shell_v1::PipCommand::Move => MenuCommand::Move,
        lxb_shell_v1::PipCommand::Resize => MenuCommand::Resize,
        lxb_shell_v1::PipCommand::Close => MenuCommand::Close,
        lxb_shell_v1::PipCommand::Realign => MenuCommand::Realign,
        _ => return None,
    })
}

/// Which corner it was asked to sit in, on the same terms.
fn pip_place_of(place: lxb_shell_v1::PipPlace) -> Option<lxb_protocol::pip::Place> {
    use lxb_protocol::pip::Place;
    Some(match place {
        lxb_shell_v1::PipPlace::TopLeft => Place::TopLeft,
        lxb_shell_v1::PipPlace::TopRight => Place::TopRight,
        lxb_shell_v1::PipPlace::BottomLeft => Place::BottomLeft,
        lxb_shell_v1::PipPlace::BottomRight => Place::BottomRight,
        _ => return None,
    })
}

/// Send one display's whole floating window list, ending with the done event
/// that makes the batch replace whatever the shell knew before.
///
/// Deliberately not part of `send_output_windows`, though it looks like it:
/// these windows are the ones that are *not* in that list — not something to
/// switch to, not something to close, not a card in the guide — and a shell
/// that read one batch as the other would offer the user their own video as an
/// application to come back to.
fn send_output_pip(shell: &LxbShellV1, output: &Output, windows: &[FloatingEntry]) -> bool {
    if shell.version() < PIP_PAD_SINCE {
        return false;
    }
    let Some(client) = shell.client() else {
        return false;
    };
    let mut sent = false;
    for wl_output in output.client_outputs(&client) {
        for entry in windows {
            shell.pip_window(
                &wl_output,
                entry.id,
                entry.rect.x.round() as i32,
                entry.rect.y.round() as i32,
                entry.rect.w.round() as i32,
                entry.rect.h.round() as i32,
            );
        }
        shell.pip_windows_done(&wl_output);
        sent = true;
    }
    sent
}

/// Send one display's whole window list, ending with the done event that
/// makes the batch replace whatever the shell knew before.
/// Tell one shell what is being kept off the screen.
///
/// Always ended with `unseen_windows_done`, empty batch included: a done event
/// with nothing before it is how "nothing is hidden any more" is said, and
/// without it a shell that had been told about a window would go on believing
/// in it.
fn send_unseen_windows(shell: &LxbShellV1, windows: &[UnseenEntry]) {
    if shell.version() < UNSEEN_WINDOWS_SINCE {
        return;
    }
    for entry in windows {
        shell.unseen_window(entry.id, entry.app_id.clone(), entry.title.clone());
    }
    shell.unseen_windows_done();
}

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
            // And behind that, who drew it. Sent on the same terms as the
            // name and for the same reason it is separate: a window whose
            // process the compositor cannot find out is left without one
            // rather than given a zero, so that "nobody said" and "process
            // number nothing" cannot be read as the same answer.
            if shell.version() >= WINDOW_PID_SINCE {
                if let Some(pid) = entry.pid {
                    shell.output_window_pid(&wl_output, entry.id, pid);
                }
            }
        }
        shell.output_windows_done(&wl_output);
        sent = true;
    }
    sent
}

impl LxbState {
    /// Draw what this compositor is putting on one side of the shell's own
    /// surfaces into a buffer the shell handed over, and say when it is done.
    ///
    /// **The shell draws glass, and glass shows what is behind it.** What the
    /// shell drew itself it reads back out of its own frame; its wallpaper it
    /// evaluates, because it is the same function that painted it. Another
    /// client it can do neither with, so without this a pane standing over a
    /// game or over somebody's video reproduces the wallpaper instead — a
    /// picture that is not behind it. See `lxb_shell_v1.ask_for_the_picture_behind`.
    ///
    /// Done here and now rather than on the way to a frame, which is what makes
    /// it free when nobody wants one: the work happens once per ask and a shell
    /// that is not drawing glass over anything does not ask. The picture it gets
    /// is therefore of the moment it asked, one frame before it uses it — which
    /// a pane frosts into invisibility.
    ///
    /// A picture that cannot be drawn is answered with a size of zero rather
    /// than an error. A pane refracting a stale game is worse than a pane
    /// refracting nothing, and neither is worth ending a session over.
    fn draw_the_picture_behind(
        &mut self,
        shell: &LxbShellV1,
        output: &Output,
        layer: smithay::reexports::wayland_server::WEnum<lxb_shell_v1::BehindLayer>,
        buffer: &WlBuffer,
    ) {
        // Which of the shell's own displays this is, from its side of the
        // connection: an event about a display is addressed with the client's
        // own object for it, as every other one here is.
        let Some(client) = shell.client() else {
            return;
        };
        let wl_outputs: Vec<_> = output.client_outputs(&client).collect();
        let asked = layer.into_result().ok();
        // An answer goes back whatever was asked for, so a shell waiting on one
        // is never left waiting. A layer this compositor does not know is
        // answered as the one below, which is the harmless direction.
        let layer = asked.unwrap_or(lxb_shell_v1::BehindLayer::Below);
        let say = |shell: &LxbShellV1, width, height| {
            for wl_output in &wl_outputs {
                shell.the_picture_behind(wl_output, layer, width, height);
            }
        };
        let Some(side) = asked.map(crate::capture::Side::from_wire) else {
            tracing::warn!("the shell asked for a picture of nowhere");
            say(shell, 0, 0);
            return;
        };
        // The display has to still be one of ours: an output resource outlives
        // the connector by however long it takes the client to hear about it.
        if !self.lxb.space.outputs().any(|known| known == output) {
            say(shell, 0, 0);
            return;
        }
        // How large a picture the shell asked for is how large a buffer it sent.
        let asked = match smithay::wayland::shm::with_buffer_contents(buffer, |_, _, data| {
            (data.width, data.height, data.format)
        }) {
            Ok(asked) => asked,
            Err(err) => {
                tracing::warn!(
                    ?err,
                    "the shell offered something that is not shared memory"
                );
                say(shell, 0, 0);
                return;
            }
        };
        let (width, height, format) = asked;
        if !matches!(format, wl_shm::Format::Abgr8888 | wl_shm::Format::Xbgr8888) {
            tracing::warn!(
                ?format,
                "the shell offered a buffer in a format with no picture in it"
            );
            say(shell, 0, 0);
            return;
        }

        let size = smithay::utils::Size::from((width, height));
        let shot = match self.backend.picture_behind(&self.lxb, output, side, size) {
            Ok(shot) => shot,
            Err(err) => {
                tracing::debug!(?err, display = %output.name(), "no picture to draw behind the shell");
                say(shell, 0, 0);
                return;
            }
        };

        // Into the shell's buffer, a row at a time: a buffer's stride is its own
        // business and is not always its width. Nothing is swizzled on the way
        // — `LAYER_FORMAT` is the order the renderer reads a picture back in,
        // chosen so that this copy is the only thing between the two.
        let written =
            smithay::wayland::shm::with_buffer_contents_mut(buffer, |slice, len, data| {
                let stride = data.stride as usize;
                let rows = shot.height.min(data.height.max(0) as u32) as usize;
                let columns = shot.width.min(data.width.max(0) as u32) as usize;
                let mut drawn = 0;
                for row in 0..rows {
                    let from = row * shot.width as usize * 4;
                    let to = data.offset.max(0) as usize + row * stride;
                    if to + columns * 4 > len || from + columns * 4 > shot.rgba.len() {
                        break;
                    }
                    // SAFETY: the slice is the client's own mapping, and both ends
                    // of this row were bounds-checked against it just above.
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            shot.rgba.as_ptr().add(from),
                            slice.add(to),
                            columns * 4,
                        );
                    }
                    drawn += 1;
                }
                (columns as i32, drawn)
            });
        match written {
            Ok((width, height)) => say(shell, width, height),
            Err(err) => {
                tracing::warn!(?err, "could not write the picture into the shell's buffer");
                say(shell, 0, 0);
            }
        }
    }

    /// Tell the shell the user asked for its overlay.
    pub fn open_guide(&mut self) {
        if !self.lxb.shell_control.has_shell() {
            tracing::debug!("guide binding pressed but no shell has bound lxb_shell_v1");
            return;
        }
        self.lxb.shell_control.send_guide();
    }

    /// One press of the key that walks the session's applications, with the
    /// modifier still held down.
    ///
    /// The shell is asked because the walk is something to be *looked* at: the
    /// pictures of what is running are the guide's window deck, and the guide
    /// is the shell's. What is settled here is only that a walk is under way,
    /// so that the modifier coming up has something to end.
    ///
    /// A session whose shell cannot draw one is not left without the chord.
    /// The compositor rotates its own stack instead — the same thing Super+Tab
    /// does, unseen and one window at a time — which is what this key did on
    /// every session before there was a deck to show for it.
    pub fn switch_window(&mut self, back: bool) {
        if !self.lxb.shell_control.wants_switch() {
            tracing::debug!("no shell can draw the deck; rotating the stack instead");
            self.cycle_window();
            return;
        }
        self.lxb.window_switch.begin();
        self.lxb.shell_control.send_switch(if back {
            lxb_shell_v1::SwitchDirection::Back
        } else {
            lxb_shell_v1::SwitchDirection::Forward
        });
    }

    /// The modifier came up, so the walk is over and what it landed on is what
    /// the user meant.
    ///
    /// Only ever reached from a walk that was under way — see
    /// [`crate::input::WindowSwitch`] — so nothing is asked here about whether
    /// there was one.
    pub fn finish_switch(&mut self) {
        self.lxb.shell_control.send_switch_done();
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

    /// Tell the shell a volume key was pressed.
    ///
    /// Forwarded rather than carried out here for the same reason the
    /// screenshot is: the compositor has the key and the shell has the mixer.
    /// It is the shell that worked out which sound server this machine is
    /// running, that holds where the control stands, and that has a bar to
    /// show it on — a compositor setting the volume behind its back would be a
    /// second opinion about it.
    pub fn change_volume(&mut self, change: lxb_shell_v1::VolumeChange) {
        if !self.lxb.shell_control.wants_volume() {
            tracing::debug!("volume key pressed but no shell is listening for it");
            return;
        }
        self.lxb.shell_control.send_volume(change);
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
            .or_else(|| self.shell_driven_output())
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

    /// Fade every display to black, or take that black back off.
    ///
    /// The session stops taking input for as long as the curtain is anything
    /// but fully up — see [`crate::curtain`], which holds both halves of this
    /// and says why the compositor rather than the shell draws it.
    ///
    /// Nothing else changes. No application is stopped, resized or told
    /// anything: what is behind the black goes on exactly as it was, which is
    /// what makes taking the curtain back up put the session back rather than
    /// restart it.
    pub fn cover_the_session_in_black(&mut self, covered: bool) {
        tracing::info!(covered, "the shell asked for the curtain");
        self.lxb.curtain.cover(covered, std::time::Instant::now());
        // Nothing else is going to ask for this frame: the picture the curtain
        // covers may be a game that is drawing anyway, or a session sitting
        // still on the start screen with nothing to say.
        self.queue_redraw();
    }

    /// Say so once the black is on every display, which is what the shell is
    /// waiting for before it ends the machine.
    ///
    /// Asked once a pass of the session's loop rather than from the render
    /// path, for the reason the flashes are pruned there: a frame is drawn per
    /// display and this is a question about all of them at once.
    pub fn tell_the_shell_when_the_screen_is_black(&mut self) {
        let displays: Vec<Output> = self.lxb.space.outputs().cloned().collect();
        if self.lxb.curtain.everything_is_black(displays.into_iter()) {
            tracing::info!("the screen is black; the shell is told");
            self.lxb.shell_control.send_screen_is_black();
        }
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

    /// Say what is being kept off the screen, and forget the exceptions whose
    /// windows have gone.
    ///
    /// Free on a session that hides nothing, which is nearly all of them: two
    /// empty sets are all it looks at, and the empty list it then says is
    /// compared against the last one said and sent only when that was not
    /// empty. That one send is not optional: a shell that has just given an
    /// application back reads its own copy of this list to see which windows
    /// the application *had*, and a list that stopped being updated the moment
    /// there was nothing to hide is a list still naming windows as hidden that
    /// are on the screen. A session that *is* hiding something walks its
    /// windows once a pass, which is the same walk the overview already does
    /// beside it.
    fn refresh_unseen_windows(&mut self) {
        if self.lxb.unseen.is_empty() && self.lxb.seen_anyway.is_empty() {
            self.lxb.shell_control.broadcast_unseen_windows(Vec::new());
            return;
        }
        // Every window there is, not the ones on a display: a hidden window is
        // deliberately not reported as being on one, so asking per display
        // would ask about a list it has been taken out of.
        let windows: Vec<(u32, bool)> = self
            .lxb
            .space
            .elements()
            .map(|window| {
                (
                    crate::overview::window_id(window),
                    self.lxb.out_of_sight(window),
                )
            })
            .collect();
        // An exception outlives nothing. A window that has gone takes its own
        // with it, so a shell that answered a question and never said anything
        // more is not what keeps the next window through.
        self.lxb
            .seen_anyway
            .retain(|id| windows.iter().any(|(seen, _)| seen == id));

        let unseen = self
            .lxb
            .space
            .elements()
            .filter(|window| self.lxb.out_of_sight(window))
            .map(|window| UnseenEntry {
                id: crate::overview::window_id(window),
                app_id: window_app_id(window),
                title: window_title(window),
            })
            .collect();
        self.lxb.shell_control.broadcast_unseen_windows(unseen);
    }

    /// Let one window of a hidden application through, or put it back.
    ///
    /// The whole of the decision is the shell's: this records the exception and
    /// then makes the screen agree with it, exactly as
    /// [`LxbState::keep_out_of_sight`] does for the application. A window that
    /// has just been let through may need the keyboard — it is very likely a
    /// question somebody has to type an answer into — and one put back cannot
    /// keep it.
    ///
    /// Naming a window that does not exist is not an error. The shell is
    /// answering a list it was sent, and a window can go between the two.
    pub fn let_this_window_be_seen(&mut self, id: u32, seen: bool) {
        let changed = if seen {
            self.lxb.seen_anyway.insert(id)
        } else {
            self.lxb.seen_anyway.remove(&id)
        };
        if !changed {
            return;
        }
        tracing::info!(
            window = id,
            seen,
            "the shell made an exception for a window"
        );
        self.focus_topmost_window();
        self.refresh_foreground();
        self.queue_redraw();
    }

    /// Draw applications this much larger than life on every display that has
    /// not been given a size of its own.
    ///
    /// The request that came before the per-display one, and still what an
    /// older shell says this with — see [`LxbState::set_output_application_scale`],
    /// which is the same setting with a display named. This one is the floor
    /// under it: what a screen nobody has named draws at, including a screen
    /// plugged in an hour from now.
    ///
    /// One relayout does all three parts of it: every window is configured at
    /// the size the new factor leaves it, told over `wp_fractional_scale_v1`
    /// what to fill that size with, and drawn back out over the display it is
    /// on. See [`crate::scale`], and
    /// [`crate::outputs::OutputManager::tile_window_on_output`] for where the
    /// first two are sent together.
    ///
    /// Nothing is written down. An application is started by the shell and the
    /// shell says what this is as soon as it connects, so there is no window
    /// that could ever come up at a size the two disagree about — which is what
    /// makes this unlike a mode or a night light, both of which the compositor
    /// remembers because the alternative is a black screen a second after
    /// login.
    ///
    /// A window that has been given a new size has not yet drawn one. Each
    /// client answers the configure in its own time, and until it does the
    /// picture on screen is the last one it sent, drawn into the rectangle the
    /// new factor asks for — soft for those few frames, and then right. There
    /// is no way to have it otherwise: the pixels belong to the application.
    pub fn set_application_scale(&mut self, scale: crate::scale::AppScale) {
        if !self.lxb.outputs.set_app_scale(scale) {
            return;
        }
        tracing::info!(
            percent = scale.percent(),
            "the shell changed how large applications draw"
        );
        self.relayout_for_the_new_scale();
    }

    /// The same, for one display: every application tiled onto it draws this
    /// much larger than life and the screens beside it are left alone.
    ///
    /// The whole relayout rather than this display's windows, and that is not
    /// laziness. A window belongs to a display, but which windows belong to
    /// which is the space's answer and not this request's — a window mid-move
    /// between two screens is on both of them for a frame — and re-tiling a
    /// window at the size it already has costs a configure the client
    /// recognises and nothing else. One pass over every window is the honest
    /// way to ask each of them the question this changes the answer to.
    pub fn set_output_application_scale(&mut self, output: &Output, scale: crate::scale::AppScale) {
        if !self.lxb.outputs.set_output_app_scale(output, scale) {
            return;
        }
        tracing::info!(
            display = %output.name(),
            percent = scale.percent(),
            "the shell changed how large applications draw on one display"
        );
        self.relayout_for_the_new_scale();
    }

    /// Draw one named application at a fixed number of pixels, and put that
    /// picture over whatever display it ends up on.
    ///
    /// The opposite bargain from [`LxbState::set_application_scale`] and
    /// carried out by the same relayout, which is the whole reason the two live
    /// beside each other: what changes is one entry in one table, and what has
    /// to happen afterwards is that every window is asked again how much room
    /// it is given and what it should fill that room with. See
    /// [`crate::scale::Resolution`] for what the two settings each ask of a
    /// client, and why they cannot be the same request with a different unit.
    ///
    /// `0` in either direction is the display's own size, which is both the
    /// answer for an application nobody has chosen for and the way a choice is
    /// taken back — so it removes the entry rather than storing a zero.
    ///
    /// A size larger than the display is not refused here. It is kept, and
    /// ignored for as long as no screen can show it: a shell offers sizes that
    /// fit the display the menu was raised on, and a session with two screens
    /// of different sizes is one where the same application may be able to
    /// honour it on one of them and not the other. See
    /// [`crate::outputs::OutputManager::resolution_on`], which is where a
    /// display that cannot show it falls back to its own size.
    pub fn set_application_resolution(&mut self, app_id: &str, width: u32, height: u32) {
        let wanted = crate::scale::Resolution::new(width, height);
        if !self.lxb.outputs.set_application_resolution(app_id, wanted) {
            return;
        }
        match wanted {
            Some(resolution) => tracing::info!(
                app_id,
                width = resolution.size().w,
                height = resolution.size().h,
                "the shell said how many pixels this application draws"
            ),
            None => tracing::info!(
                app_id,
                "this application draws at the size of the display again"
            ),
        }
        self.relayout_for_the_new_scale();
    }

    /// Put every window back through the tiling, at whatever size its own
    /// display now asks for, and get the result onto a screen.
    ///
    /// Shared by the two requests above so they cannot come to differ: what
    /// they change is one number each, and what has to happen afterwards is the
    /// same thing.
    fn relayout_for_the_new_scale(&mut self) {
        self.lxb.outputs.relayout_windows(&mut self.lxb.space);
        // Nothing here reaches a screen by itself: every window has been given
        // a new size and nothing has been scanned out since.
        self.queue_redraw();
    }

    /// What the session's pointing devices do from now on: how fast the pointer
    /// travels, how large it is drawn, and how far and which way a wheel
    /// carries the content under it.
    ///
    /// **Written into the config this compositor is running on**, rather than
    /// kept beside it. Three of the four are read again for every device
    /// libinput hands over — see `configure_libinput_device` — so a mouse
    /// plugged in after the shell said this gets the shell's answer without
    /// anything else being told, and a session with no shell goes on getting
    /// the file's. Two copies of "how fast is the pointer" would be two answers
    /// to it, and the one that lost would be whichever was read second.
    ///
    /// Then applied to the devices already open, because this arrives while
    /// somebody is watching the pointer they are changing.
    ///
    /// Nothing is written to disk here, for the reason
    /// [`LxbState::set_application_scale`] writes nothing: the shell says what
    /// these are as soon as it connects, and it is the shell that has the page
    /// they were set on.
    /// Set the xkb layout every keyboard on the seat types through, and tell
    /// the shell what it ended up being.
    ///
    /// **Refused rather than fallen back from.** xkbcommon compiles the names
    /// or it does not, and a machine whose keyboard silently became American is
    /// a machine somebody may not be able to type their password into. A layout
    /// that will not compile leaves the seat exactly as it was, says so in the
    /// log, and the event below tells the shell what is really in force — which
    /// is what stops a settings page showing a mark for a row that did nothing.
    ///
    /// The seat's, not a device's. A keymap belongs to the seat's keyboard and
    /// every physical board on it types through the same one; smithay sends the
    /// new keymap and modifier state to every client that holds the keyboard,
    /// so an application already running follows without being restarted.
    pub fn set_keyboard_layout(&mut self, layout: &str, variant: &str) {
        let input = &self.lxb.config.input;
        if input.keyboard_layout == layout && input.keyboard_variant == variant {
            return;
        }
        let Some(keyboard) = self.lxb.seat.get_keyboard() else {
            tracing::warn!("the shell asked for a keyboard layout and this seat has no keyboard");
            return;
        };
        // Copied out before the call: `set_xkb_config` takes the whole state,
        // and the names it is being handed are read off that state.
        let (rules, model, options) = (
            input.keyboard_rules.clone(),
            input.keyboard_model.clone(),
            input.keyboard_options.clone(),
        );
        let xkb = smithay::input::keyboard::XkbConfig {
            rules: &rules,
            model: &model,
            layout,
            variant,
            options,
        };
        if let Err(err) = keyboard.set_xkb_config(self, xkb) {
            tracing::warn!(
                layout,
                variant,
                ?err,
                "that keyboard layout will not compile; the keyboard is unchanged"
            );
            // Still told, and deliberately: the shell has just moved a mark to
            // a row this did not carry out, and the event is what puts it back.
            self.tell_the_shell_the_keyboard_layout();
            return;
        }
        let input = &mut self.lxb.config.input;
        input.keyboard_layout = layout.to_string();
        input.keyboard_variant = variant.to_string();
        tracing::info!(layout, variant, "the shell changed the keyboard layout");
        export_keyboard_environment(layout, variant);
        self.tell_the_shell_the_keyboard_layout();
    }

    /// Say what the seat's keyboards are set to.
    ///
    /// Sent when a shell binds and again after every change, including one this
    /// compositor refused: a shell cannot read this compositor's config file,
    /// so without it a settings page has nothing to mark on a machine nobody
    /// has ever set a layout on — which is every machine the first time.
    pub fn tell_the_shell_the_keyboard_layout(&mut self) {
        let input = &self.lxb.config.input;
        let (layout, variant) = (
            input.keyboard_layout.clone(),
            input.keyboard_variant.clone(),
        );
        for instance in &self.lxb.shell_control.instances {
            if instance.version() >= LAYOUT_SINCE {
                instance.keyboard_layout(layout.clone(), variant.clone());
            }
        }
    }

    pub fn set_pointer(&mut self, speed: i32, size: u32, scroll: u32, natural: bool) {
        // libinput's own range, in the units the protocol carries it in. Out of
        // range is clamped rather than refused, as a colour temperature is: a
        // pointer at some unrelated speed is a worse way to report a bad number
        // than the nearest one that means something.
        let accel = f64::from(speed.clamp(-100, 100)) / 100.0;
        let scroll = f64::from(scroll.clamp(1, 1000)) / 100.0;
        let input = &mut self.lxb.config.input;
        let changed = input.pointer_accel != accel
            || input.natural_scroll != natural
            || input.scroll_speed != scroll;
        input.pointer_accel = accel;
        input.natural_scroll = natural;
        input.scroll_speed = scroll;
        if changed {
            tracing::info!(
                accel,
                natural,
                scroll,
                "the shell changed what the pointing devices do"
            );
            let config = self.lxb.config.input.clone();
            self.backend.apply_input_settings(&config);
        }

        // The cursor is the backend's own picture rather than a device setting,
        // so it is set whatever the three above did — and it is the one of the
        // four that needs a frame: nothing has been scanned out since the
        // pointer changed size, and a cursor that waited for the next mouse
        // movement to grow would read as a press that half worked.
        //
        // Zero means "say nothing", which is what an unset `cursor_size`
        // already means to this compositor. [`CursorState::set_size`] ignores
        // it rather than drawing a pointer nought pixels across.
        if size > 0 && self.lxb.config.general.cursor_size != Some(size) {
            self.lxb.config.general.cursor_size = Some(size);
            self.backend.set_cursor_size(size);
            // And every client started from now on, so an application drawing
            // its own pointer out of its own theme draws it at the size the
            // rest of the session is using. It cannot reach one already
            // running: a toolkit reads this when it starts.
            crate::cursor::export_cursor_environment(
                self.lxb.config.general.cursor_theme.as_deref(),
                Some(size),
            );
            self.queue_redraw();
        }
    }

    /// Float a browser's picture-in-picture window from now on, at this size and
    /// in this corner — or stop floating it.
    ///
    /// A value the compositor has no meaning for leaves that half of the answer
    /// where it is, which is what makes a shell built against a later version
    /// of this protocol safe to run: it asks for a fourth corner, gets the one
    /// it already had, and the window stays somewhere sensible.
    ///
    /// One layout does all of it. The window that floats is given the corner,
    /// and every window that has stopped floating — the case of turning the
    /// feature off — is given its display back, both by the same call in
    /// [`crate::outputs::OutputManager::tile_window_on_output`].
    ///
    /// Nothing is written down here, for the reason
    /// [`LxbState::set_application_scale`] writes nothing down: the shell says
    /// what this is as soon as it connects, and it says it long before any
    /// application exists to put a video in.
    pub fn set_picture_in_picture(
        &mut self,
        floating: bool,
        size: Option<lxb_protocol::pip::Size>,
        place: Option<lxb_protocol::pip::Place>,
    ) {
        let held = self.lxb.outputs.pip().settings();
        let settings = crate::pip::Settings {
            floating,
            size: size.unwrap_or(held.size),
            place: place.unwrap_or(held.place),
        };
        if !self.lxb.outputs.set_pip(settings) {
            return;
        }
        tracing::info!(
            floating,
            size = settings.size.key(),
            place = settings.place.key(),
            "the shell set what a picture-in-picture window does"
        );
        // Every window somebody had dragged out of the column goes back into
        // it. A press on the Settings page is the user saying where they want
        // their videos; a window left in the middle of the screen because it
        // was once dragged there would be the session ignoring them. Nothing
        // else puts one back — see [`crate::pip::Floating::reattach`].
        for window in self.lxb.space.elements() {
            if self.lxb.floating(window) {
                crate::pip::floating_state(window).reattach();
            }
        }
        self.lxb.outputs.relayout_windows(&mut self.lxb.space);
        // A window that has just stopped floating is an ordinary window again
        // and may well be the one in front now; one that has just started
        // floating must not be holding the keyboard.
        self.focus_topmost_window();
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

        // And whether each display has an application in front of it at all,
        // which is the same window asked about a third time — whether there is
        // one. A display where the answer is no is showing the shell and
        // nothing else, which is the still picture OLED protection exists for.
        let per_output_in_use = outputs
            .iter()
            .map(|output| {
                let in_use = self.topmost_application(Some(output)).is_some();
                (output.clone(), in_use)
            })
            .collect();
        self.lxb
            .shell_control
            .broadcast_output_in_use(per_output_in_use);

        // And whether anything on each display is still painting. A reading of
        // the clock rather than of the window stack, so it rides here for the
        // plainest reason: this is the refresh that runs every pass of the
        // session's loop.
        self.refresh_output_drawing();

        // The overview's window lists ride the same refresh: they are diffed
        // per display, so an unchanged desktop sends nothing.
        let per_output_windows = outputs
            .into_iter()
            .map(|output| {
                let windows = crate::render::overview_windows(&self.lxb, &output)
                    .into_iter()
                    .map(|window| {
                        // What the window occupies on the display, not what its
                        // client was configured at. The two differ by the whole
                        // of the application scale, and the shell decides
                        // whether to go on drawing by comparing this against the
                        // size of the display — see [`crate::render::on_screen`],
                        // which is the same rectangle the frame throttle here
                        // reads, so that the two sides cannot answer "does this
                        // cover the screen" differently.
                        let size = crate::render::on_screen(&self.lxb, &window)
                            .map(|geometry| geometry.size)
                            .unwrap_or_default();
                        OverviewEntry {
                            id: crate::overview::window_id(&window),
                            title: window_title(&window),
                            app_id: window_app_id(&window),
                            // A negative pid is not a process; the two ways
                            // of not knowing are the same answer here.
                            pid: self
                                .window_pid(&window)
                                .and_then(|pid| u32::try_from(pid).ok())
                                .filter(|pid| *pid != 0),
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

        // And the windows that are in none of those lists because the shell
        // asked for them to be in none of them. See
        // `lxb_shell_v1.unseen_window`.
        self.refresh_unseen_windows();

        // And the windows that are deliberately absent from that list: the ones
        // floating over it. A shell driven by a controller has no pointer to
        // reach one with, so it has to be told where they are — see
        // `lxb_shell_v1.pip_window`. Diffed like the rest, and the diff earns
        // its keep here: while a window is being dragged this is a fresh list
        // every pass, and while one is merely sitting in a corner it is the same
        // list for as long as the video plays.
        let outputs: Vec<Output> = self.lxb.space.outputs().cloned().collect();
        let per_output_pip = outputs
            .into_iter()
            .map(|output| {
                let windows = self
                    .lxb
                    .space
                    .elements_for_output(&output)
                    .rev()
                    .filter(|window| self.lxb.floating(window) && !self.lxb.out_of_sight(window))
                    .filter_map(|window| {
                        Some(FloatingEntry {
                            id: crate::overview::window_id(window),
                            rect: crate::pip::floating_state(window).frame()?.outer,
                        })
                    })
                    .collect();
                (output, windows)
            })
            .collect();
        self.lxb.shell_control.broadcast_output_pip(per_output_pip);

        // What each display can do in HDR rides along too, for the same
        // reason: it is diffed, so a session where nobody touches it never
        // sends a second event.
        self.refresh_hdr();

        // Those broadcasts only carry changes, so a shell whose outputs were
        // not ready when it bound would otherwise wait for the desktop to
        // change before learning what is on it.
        self.lxb.shell_control.catch_up_new_shells();
    }

    /// Publish which displays still have something painting on them.
    ///
    /// [`crate::render::windows_on_screen`] is what "on this display" means
    /// here, and it is the same answer the sleep pass uses: a window behind a
    /// fullscreen one is not on screen, and a display the shell's own opaque
    /// surface covers has nothing on it at all — which is the start screen, and
    /// is the still picture this whole rule exists for.
    fn refresh_output_drawing(&mut self) {
        let outputs: Vec<Output> = self.lxb.space.outputs().cloned().collect();
        let current = outputs
            .into_iter()
            .map(|output| {
                let drawing = crate::render::windows_on_screen(&self.lxb, &output)
                    .iter()
                    .any(|window| crate::render::painted_within(window, STILL));
                (output, drawing)
            })
            .collect();
        self.lxb.shell_control.broadcast_output_drawing(current);
    }

    /// Rest one display behind black, or bring it back.
    ///
    /// The shell decides when — the setting is its Settings column's, and so is
    /// every fact about where the user's attention is. What is here is the
    /// sheet and nothing else. See [`crate::blackout`].
    pub fn cover_output_in_black(&mut self, output: &Output, covered: bool) {
        tracing::debug!(display = %output.name(), covered, "the shell rested a display");
        self.lxb
            .blackouts
            .cover(output, covered, std::time::Instant::now());
        // Nothing else is going to ask for this frame: the display being
        // covered is showing a start screen that has stopped animating,
        // precisely because the shell has stopped drawing it.
        self.queue_redraw();
    }

    /// Publish each display's HDR capability and state, if either changed.
    ///
    /// Called both from the refresh above and straight from the backend the
    /// moment a commit lands, because the answer to "did that work" has to
    /// reach the Settings column the user is looking at rather than wait for
    /// the next time a window moves.
    pub fn refresh_hdr(&mut self) {
        // Before the shell guard below, and deliberately: an application that
        // asked what its display is being driven as is owed the answer whether
        // or not a shell is bound. On a session started without one — every
        // nested debugging run — the guard would otherwise swallow the only
        // notification a colour-managed client ever gets.
        crate::colour_management::displays_changed(self);

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

    /// Publish where each display stands in the arrangement, if any of them
    /// moved.
    ///
    /// Called on the same occasions the orientations are, plus the one that is
    /// this event's own: a display arriving or leaving renumbers every display
    /// laid out after it, without anybody having asked for anything.
    ///
    /// Built from the compositor's own order rather than from the space's list
    /// of outputs, because that order *is* the answer — see
    /// [`crate::outputs::OutputManager::placed`], which is also what leaves out
    /// the displays whose place is not this compositor's to set.
    pub fn refresh_places(&mut self) {
        if !self.lxb.shell_control.has_shell() {
            return;
        }
        let places = self
            .lxb
            .outputs
            .placed(&self.lxb.config)
            .into_iter()
            .enumerate()
            .map(|(place, output)| (output, place))
            .collect();
        self.lxb.shell_control.broadcast_output_place(places);
    }

    /// The window an overview id names, if it is still mapped.
    pub(crate) fn window_by_overview_id(&self, id: u32) -> Option<Window> {
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
        // Before anything is sent: a `SIGTERM` to a stopped process is a signal
        // pending on something that will never run to handle it, and the whole
        // point of sending it first is that the application gets to finish what
        // it was doing. See [`LxbState::wake_this_application`].
        self.wake_this_application(window);
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
        // Times how much larger than life the application is drawing, for
        // exactly the same reason — a window scaled to 150% put a buffer half
        // again as wide on the screen, and a photograph of it that ignored
        // that would be the one picture of this window nobody ever saw.
        let scale = match self.lxb.outputs.window_display(&self.lxb.space, &window) {
            Some(output) => {
                output.current_scale().fractional_scale()
                    * self.lxb.outputs.window_scale_on(&window, &output)
            }
            None => 1.0,
        };

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
    pub(crate) fn window_pid(&self, window: &Window) -> Option<i32> {
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
    pub(crate) fn topmost_application(&self, output: Option<&Output>) -> Option<Window> {
        self.lxb
            .space
            .elements()
            .rev()
            .find(|window| {
                self.lxb.takes_the_keyboard(window)
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

    /// The display the user is driving, if the shell has said and it is still
    /// connected.
    ///
    /// Validated the same way and for the same reason as the launch output: a
    /// display can be unplugged between the shell naming it and this being
    /// asked.
    ///
    /// Falls back to the launch output, which is what a shell older than
    /// version 41 says instead — one value answering both questions. That
    /// fallback is exactly the behaviour this compositor had before the two
    /// came apart, so an old shell is neither better nor worse off.
    pub fn shell_driven_output(&self) -> Option<Output> {
        let wanted = the_display_being_driven(
            self.lxb.shell_control.driven_output(),
            self.lxb.shell_control.launch_output(),
        )?;
        self.lxb
            .space
            .outputs()
            .find(|output| *output == wanted)
            .cloned()
    }

    /// The display one newly mapped window's own launch was started from, if
    /// the shell filed a record this window answers to.
    ///
    /// Asked before the session-wide launch output, and it is the whole of what
    /// keeps two displays loading two things from opening them on one screen.
    /// A window that matches nothing falls through to that older answer, so a
    /// session where the shell files nothing places windows exactly as it
    /// always did.
    ///
    /// The newest matching record wins. Two records can name one program — the
    /// same game pressed twice on two screens, an application started again
    /// before the first arrived — and the later press is the one whose window
    /// has had the least time to turn up. It is the same rule the shell's own
    /// single answer used, applied where it now belongs.
    pub(crate) fn launch_output_for(&self, window: &Window) -> Option<Output> {
        if self.lxb.shell_control.launches.is_empty() {
            return None;
        }
        let pid = self
            .window_pid(window)
            .and_then(|pid| u32::try_from(pid).ok());
        let app_id = window_app_id(window);
        let matched = launch_for(
            &self.lxb.shell_control.launches,
            std::time::Instant::now(),
            pid,
            &app_id,
            descends_from,
        )?;
        // Validated at the moment it is read, like every other output the
        // shell names: the display a launch was filed against can be unplugged
        // while the application it started is still coming up.
        let output = self
            .lxb
            .space
            .outputs()
            .find(|output| **output == matched.output)
            .cloned();
        if output.is_none() {
            tracing::debug!(
                launch = matched.launch,
                "this launch's display went away before its window arrived"
            );
        }
        output
    }
}

/// Whether `pid` is `ancestor`, or was started by it however far down.
///
/// A window's process is very often not the process the shell forked. A
/// desktop entry runs a shell script, a game runs under a container and a
/// runtime and a compatibility tool, a launcher execs the thing it launched —
/// and every one of those is the same launch as far as anybody watching the
/// screen is concerned. So the chain is walked rather than the pid compared.
///
/// Read straight off `/proc`, which is the only place this answer exists, and
/// bounded: a corrupt or racing chain must not spin. Anything it cannot read —
/// a process that exited between the window mapping and this being asked, a
/// kernel without `/proc` — is not a match, which puts the window back on the
/// session's own answer rather than on a guess.
fn descends_from(pid: u32, ancestor: u32) -> bool {
    /// Deeper than any launch this session makes; a chain longer than this is
    /// one that is not going to end.
    const AS_FAR_AS_A_LAUNCH_GOES: usize = 32;
    let mut walking = pid;
    for _ in 0..AS_FAR_AS_A_LAUNCH_GOES {
        if walking == ancestor {
            return true;
        }
        if walking <= 1 {
            return false;
        }
        let Some(parent) = parent_of(walking) else {
            return false;
        };
        walking = parent;
    }
    false
}

/// The parent of one process, out of `/proc/<pid>/stat`.
///
/// Parsed from the last `)` rather than by splitting on spaces: the second
/// field is the executable's name in brackets and a program is free to have
/// spaces and brackets in its name — which is not a curiosity, it is what a
/// game's own launcher is called half the time.
fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_the_name = stat.rsplit_once(')')?.1;
    // What follows the name is " R <ppid> …": the state, then the parent.
    after_the_name.split_whitespace().nth(1)?.parse().ok()
}

/// A window's human-readable title, falling back to something the shell can
/// still put in a menu when the client set none.
pub(crate) fn window_title(window: &Window) -> String {
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
    /// Whether this client may see the global at all.
    ///
    /// The one hook that does both halves of it: a client this refuses is not
    /// told the global exists, and an attempt to bind it by number anyway is a
    /// protocol error rather than a bound object. See [`may_drive_the_shell`].
    fn can_view(client: Client, _global_data: &()) -> bool {
        let allowed = may_drive_the_shell(&client);
        if !allowed {
            let who = client.get_data::<crate::state::ClientState>();
            tracing::debug!(
                pid = ?who.and_then(|who| who.pid),
                program = ?who.and_then(|who| who.program.as_deref()),
                "not offering lxb_shell_v1 to this client"
            );
        }
        allowed
    }

    fn bind(
        state: &mut Self,
        _handle: &DisplayHandle,
        client: &Client,
        resource: New<LxbShellV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        // Belt as well as braces. `can_view` is what actually refuses, and it
        // refuses in the registry where the client can neither see nor bind
        // this; reaching here without it having said yes would mean the
        // guarantee had moved somewhere else, and the cost of noticing that
        // late is every request below.
        let Some(role) = role_of(client) else {
            tracing::warn!("refusing lxb_shell_v1 to a client that may not drive the session");
            return;
        };
        let shell = data_init.init(resource, ());

        // The portal gets the object and nothing else: no state, and none of
        // the session-wide work below. It binds this to ask one question and
        // hear one answer, and everything the shell is told on binding is a
        // description of what the user has on screen.
        if role == Role::Portal {
            state.lxb.shell_control.portals.push(shell);
            tracing::info!("the session portal bound lxb_shell_v1");
            return;
        }

        // A shell that binds late still needs to know what is on screen, and
        // the broadcasts only carry changes. This early it will usually have
        // no `wl_output` bound yet, so it is queued for another try.
        if !state.lxb.shell_control.send_current(&shell) {
            state.lxb.shell_control.awaiting_outputs.push(shell.clone());
        }
        state.lxb.shell_control.instances.push(shell);
        tracing::info!("session shell bound lxb_shell_v1");

        // Whatever the last shell was told about the user's hands was told to
        // it and not to this one, which has just started and knows nothing: the
        // next key pressed is news again. Otherwise a shell restarted after a
        // spell of typing would go on offering a keyboard to somebody sitting
        // at one, with nothing left that could ever say so.
        state.lxb.shell_control.typing_is_news_again();

        // The first instance turns foreground tracking on; publish the current
        // window straight away rather than waiting for the next change.
        state.refresh_foreground();
        // And what every display can be driven at, how each one's picture is
        // turned, and where each one stands, which are otherwise only published
        // when a display is plugged in — on a session that started with its
        // displays already there, that is never.
        state.refresh_modes();
        state.refresh_transforms();
        state.refresh_places();
        // And what the keyboard is set to, which no other process in the
        // session can find out: it is this compositor's own configuration file,
        // and the shell's settings page would otherwise have nothing to mark
        // the first time it was ever opened.
        state.tell_the_shell_the_keyboard_layout();
    }
}

/// Tell the session's children which keyboard they are being typed on.
///
/// `XKB_DEFAULT_LAYOUT` and `XKB_DEFAULT_VARIANT` are what libxkbcommon reads
/// when nothing hands it a keymap, so this is how a program that draws its own
/// picture of a keyboard finds out what to print on it — the login screen's
/// board, and the one a toolkit application raises for a search field. Both
/// fall back to this machine's X11 keyboard configuration without it, which is
/// right until somebody changes the layout on the Settings page and only one of
/// the two answers moves.
///
/// Exactly the bargain [`crate::cursor::export_cursor_environment`] is under,
/// and with the same limitation said out loud: a process reads these when it
/// starts, so this reaches the next client and cannot reach one already
/// running. Nothing that matters depends on it — the seat's own keymap is set
/// either way, and every Wayland client is sent that.
pub fn export_keyboard_environment(layout: &str, variant: &str) {
    // SAFETY: single-threaded startup and event loop; nothing else in this
    // process reads the environment concurrently. The same argument the cursor
    // export is made under.
    unsafe {
        std::env::set_var("XKB_DEFAULT_LAYOUT", layout);
        match variant.is_empty() {
            // Removed rather than set empty: an empty variant is what "the
            // layout's own arrangement" means, and a stale one left behind
            // would be a different keyboard.
            true => std::env::remove_var("XKB_DEFAULT_VARIANT"),
            false => std::env::set_var("XKB_DEFAULT_VARIANT", variant),
        }
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
        // Which half of the interface this object reaches. `can_view` decided
        // whether the client may be here at all, from the pid on its socket;
        // this decides what it may ask for now that it is. A portal reaching
        // past its three requests is a portal that has been taken over, or one
        // that has grown a use for the shell's half and should be discussed
        // rather than quietly allowed — either way it is a protocol error and
        // not a silent no, so it cannot be mistaken for a request that worked.
        if state.lxb.shell_control.is_portal(resource) && !portal_may_ask(&request) {
            tracing::warn!(
                ?request,
                "the portal asked for something only the shell may ask"
            );
            resource.post_error(
                lxb_shell_v1::Error::NotPermitted,
                "this request belongs to the session shell",
            );
            return;
        }
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
                // The shell has moved of its own accord, so what it was last
                // told about a press is spent: a press back on the display it
                // has just left is news again. See
                // [`ShellControlState::pressed_output`].
                state.lxb.shell_control.pressed_output = None;
            }
            lxb_shell_v1::Request::SetDrivenOutput { output } => {
                let output = Output::from_resource(&output);
                tracing::debug!(
                    display = output.as_ref().map(|o| o.name()).unwrap_or_default(),
                    "shell said which display is being driven"
                );
                state.lxb.shell_control.driven_output = output;
                // The same reason `set_launch_output` spends it: the shell has
                // said where the user is, so a press reported on the display
                // they have just left is news again.
                state.lxb.shell_control.pressed_output = None;
            }
            lxb_shell_v1::Request::PlaceLaunch {
                launch,
                output,
                pid,
                app_id,
            } => {
                let Some(output) = Output::from_resource(&output) else {
                    // The display went away between the press and this
                    // reaching the compositor. Nothing to file: a record
                    // naming an output that does not exist would never match
                    // anything, and the window falls back to the session's
                    // own answer either way.
                    tracing::debug!(launch, "a launch was placed on a display that is gone");
                    return;
                };
                let app_id = Some(app_id).filter(|name| !name.trim().is_empty());
                tracing::debug!(
                    launch,
                    display = %output.name(),
                    pid,
                    app_id = app_id.as_deref().unwrap_or_default(),
                    "shell filed a launch"
                );
                state.lxb.shell_control.place_launch(LaunchRecord {
                    launch,
                    output,
                    // Zero is the protocol's "no process", and it is also the
                    // one pid nothing can be descended from, so it is dropped
                    // here rather than matched against later.
                    pid: Some(pid).filter(|pid| *pid != 0),
                    app_id,
                    filed: std::time::Instant::now(),
                });
            }
            lxb_shell_v1::Request::ForgetLaunch { launch } => {
                tracing::debug!(launch, "shell took a launch back");
                state.lxb.shell_control.forget_launch(launch);
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
                        // And what the pointer is over, which has just changed
                        // under a pointer that did not move.
                        state.refresh_pointer_focus();
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
                        state.refresh_pointer_focus();
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
            lxb_shell_v1::Request::SetOutputPlace { output, place } => {
                match Output::from_resource(&output) {
                    Some(output) => {
                        // Nothing is clamped here, for the reason a mode is
                        // not: a place past the end of the list is not a
                        // request with a sensible neighbour, it is a shell
                        // describing an arrangement this compositor does not
                        // have, and putting the display at the far end instead
                        // would be inventing an answer.
                        if state.set_output_place(&output, place as usize) {
                            // Where every display now stands, for the page that
                            // asked — and for the page on the display it traded
                            // with, which moved without being asked.
                            state.refresh_places();
                        }
                    }
                    None => tracing::debug!("a display that is gone was asked to move"),
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
            lxb_shell_v1::Request::OfferKind {
                id,
                name,
                pattern,
                matching,
            } => {
                // How the pattern is read is passed on and never applied: it is
                // the shell that matches a file, and a compositor that started
                // vetting file types would be a second opinion about what an
                // image is. What it cannot pass on is a number the protocol has
                // no name for, which is the asking client at fault rather than
                // the user, so the kind is dropped and the question still goes.
                match matching.into_result() {
                    Ok(matching) => state
                        .lxb
                        .shell_control
                        .send_pick_kind(resource, id, &name, &pattern, matching),
                    Err(unknown) => {
                        tracing::warn!(
                            id,
                            ?unknown,
                            "a file kind offered in a way this protocol has no name for"
                        )
                    }
                }
            }
            lxb_shell_v1::Request::AskToPickFiles {
                id,
                app_id,
                purpose,
                title,
                accept,
                name,
                at,
            } => {
                tracing::info!(id, %app_id, "an application is asking for a file");
                // A purpose this protocol has no name for is the asking
                // client at fault, and the safe reading of it is the narrowest
                // one there is: a single file that is already on the disk. It
                // is never "choose a folder and a name", which is the only
                // purpose whose answer is somewhere to *write*.
                let purpose = purpose
                    .into_result()
                    .unwrap_or(lxb_shell_v1::Picking::OneFile);
                if !state
                    .lxb
                    .shell_control
                    .send_pick_request(resource, id, &app_id, purpose, &title, &accept, &name, &at)
                {
                    // Nobody to ask, so nobody chose anything. Answered here
                    // and now rather than left for a timeout somewhere else,
                    // for the reason a share is: a session with no shell is not
                    // one that is about to grow one mid-question.
                    tracing::info!(
                        id,
                        "answering with nothing: no shell to put the question to"
                    );
                    resource.pick_answered(id, NO_KIND);
                }
            }
            lxb_shell_v1::Request::ChoseFile { id, path } => {
                state.lxb.shell_control.note_chosen_file(id, path);
            }
            lxb_shell_v1::Request::AnswerPick { id, kind } => {
                tracing::info!(id, "the shell answered a file request");
                state.lxb.shell_control.send_pick_answer(id, kind);
            }
            lxb_shell_v1::Request::KeepOutOfSight { app_id, hidden } => {
                state.keep_out_of_sight(&app_id, hidden == 1)
            }
            lxb_shell_v1::Request::LetThisWindowBeSeen { id, seen } => {
                state.let_this_window_be_seen(id, seen == 1)
            }
            lxb_shell_v1::Request::KeepAwake { app_id, awake } => {
                state.keep_application_awake(&app_id, awake == 1)
            }
            lxb_shell_v1::Request::SetApplicationScale { scale } => {
                state.set_application_scale(crate::scale::AppScale::from_percent(scale))
            }
            lxb_shell_v1::Request::SetOutputApplicationScale { output, scale } => {
                match Output::from_resource(&output) {
                    Some(output) => state.set_output_application_scale(
                        &output,
                        crate::scale::AppScale::from_percent(scale),
                    ),
                    None => tracing::debug!(
                        "an application scale was asked for on a display that is gone"
                    ),
                }
            }
            lxb_shell_v1::Request::SetApplicationResolution {
                app_id,
                width,
                height,
            } => state.set_application_resolution(&app_id, width, height),
            lxb_shell_v1::Request::SetPointer {
                speed,
                size,
                scroll,
                natural,
            } => state.set_pointer(speed, size, scroll, natural == 1),
            lxb_shell_v1::Request::SetKeyboardLayout { layout, variant } => {
                state.set_keyboard_layout(&layout, &variant)
            }
            lxb_shell_v1::Request::SetPictureInPicture {
                enabled,
                size,
                place,
            } => state.set_picture_in_picture(
                enabled == 1,
                size.into_result().ok().and_then(pip_size_of),
                place.into_result().ok().and_then(pip_place_of),
            ),
            lxb_shell_v1::Request::PipMenuCommand { id, command } => {
                match command.into_result().ok().and_then(pip_command_of) {
                    Some(command) => state.pip_menu_command(id, command),
                    // A row this compositor has no meaning for, from a shell
                    // built against a later version of this protocol. Left
                    // alone rather than guessed at, exactly as an unknown size
                    // or corner is.
                    None => tracing::debug!(id, ?command, "an unknown floating window command"),
                }
            }
            lxb_shell_v1::Request::SetWindowFloating { id, floating } => {
                state.set_window_floating(id, floating == 1)
            }
            lxb_shell_v1::Request::PipSelect { id, accent } => {
                state.select_floating_window(id, accent & 0x00ff_ffff)
            }
            lxb_shell_v1::Request::PipGrab { id, handle } => {
                match handle.into_result().ok() {
                    Some(lxb_shell_v1::PipHandle::Move) => state.grab_floating_window(id, false),
                    Some(lxb_shell_v1::PipHandle::Resize) => state.grab_floating_window(id, true),
                    // A way of holding a window this compositor has never heard
                    // of, from a shell built against a later version of this
                    // protocol. Left alone rather than guessed at: the one thing
                    // worse than a grab that does nothing is one that resizes a
                    // window somebody meant to move.
                    _ => tracing::debug!(id, ?handle, "an unknown way to hold a floating window"),
                }
            }
            lxb_shell_v1::Request::PipDrag { dx, dy } => {
                state.drag_floating_window(smithay::utils::Point::from((dx, dy)))
            }
            lxb_shell_v1::Request::PipDrop { keep } => state.drop_floating_window(keep == 1),
            lxb_shell_v1::Request::SetMenuSurface { surface } => state.set_menu_surface(surface),
            lxb_shell_v1::Request::AskForThePictureBehind {
                output,
                layer,
                buffer,
            } => {
                if let Some(output) = Output::from_resource(&output) {
                    state.draw_the_picture_behind(resource, &output, layer, &buffer);
                }
            }
            lxb_shell_v1::Request::HidePointer => state.pointer_put_down(),
            lxb_shell_v1::Request::ControllerUsed => {
                // The one thing about the user's hands the compositor cannot
                // see for itself: a pad is read from `/dev/input` by the shell
                // and is not a seat device, so a thumb landing on it happens
                // entirely outside this process. The next key pressed is news
                // again. See [`ShellControlState::typing_is_news`].
                tracing::debug!("the shell says the controller is back in hand");
                state.lxb.shell_control.typing_is_news_again();
            }
            lxb_shell_v1::Request::KeyboardKey { key, state: down } => {
                // As above: anything that is not "pressed" is a release.
                let pressed = down
                    .into_result()
                    .is_ok_and(|down| down == lxb_shell_v1::KeyState::Pressed);
                state.shell_keyboard_key(key, pressed);
            }
            lxb_shell_v1::Request::CoverInBlack { covered } => {
                state.cover_the_session_in_black(covered == 1)
            }
            lxb_shell_v1::Request::CoverOutputInBlack { output, covered } => {
                match Output::from_resource(&output) {
                    Some(output) => state.cover_output_in_black(&output, covered == 1),
                    None => tracing::debug!("a display that is gone was asked to rest"),
                }
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
        state
            .lxb
            .shell_control
            .portals
            .retain(|instance| instance != resource);
        // Anything this client was waiting on is nobody's business now.
        state.lxb.shell_control.forget_shares(resource);
        state.lxb.shell_control.forget_picks(resource);
        // And anything anybody else was waiting on has lost the client that
        // could have said yes to it. An unanswered question is a no.
        //
        // Only a shell could have said yes, so it is the shells that are
        // counted. Written as "fewer than two of everything bound" while the
        // portal shared this list, which meant a portal disconnecting refused
        // the questions a perfectly live shell was still looking at.
        if !state.lxb.shell_control.has_shell() {
            state.lxb.shell_control.refuse_all_shares();
            state.lxb.shell_control.refuse_all_picks();
        }

        // With no shell left there is nobody to close the overview, and a
        // desktop whose windows are all shrunk into cards is unusable. A
        // shell that crashes or is restarted must not leave one behind.
        if !state.lxb.shell_control.has_shell() {
            state.lxb.overview.close_all(std::time::Instant::now());

            // And with no shell there is nobody left to take back what it said
            // about an application playing something. Left standing, one
            // exemption granted before a shell crashed would keep that
            // application out of the sleeper for the rest of the session, and
            // nothing on screen would ever say why.
            if !state.lxb.playing.is_empty() {
                state.lxb.playing.clear();
                tracing::info!("the shell has gone; nothing is exempt from being stopped any more");
                state.refresh_application_sleep();
            }

            // And nothing left to say an application may be seen again. These
            // outlived the client that asked for them: a shell that crashed
            // while it had Valve's client hidden left every Steam window on
            // this machine invisible for the rest of the session, including the
            // ones somebody opened for themselves afterwards, with nothing on
            // screen able to explain it. A rule with nobody behind it is not a
            // rule the compositor should still be keeping.
            if !state.lxb.unseen.is_empty() {
                let forgotten: Vec<String> = state.lxb.unseen.drain().collect();
                tracing::info!(
                    ?forgotten,
                    "the shell has gone; these applications may be seen again"
                );
                state.focus_topmost_window();
                state.refresh_foreground();
                state.queue_redraw();
            }
        }
    }
}

#[cfg(test)]
mod privilege_tests {
    use super::*;
    use smithay::reexports::wayland_server::WEnum;

    fn session(shell: i32, portal: i32) -> Privileged {
        Privileged {
            shell: Some(shell),
            portal: Some(portal),
            by_name: Vec::new(),
        }
    }

    /// The two processes this compositor started may drive it, each as itself,
    /// and nothing else may.
    #[test]
    fn only_the_processes_this_compositor_started_may_drive_it() {
        let running = session(4242, 9001);

        assert_eq!(
            role_for(Some(4242), Some("lxb-desktop"), &running),
            Some(Role::Shell)
        );
        assert_eq!(
            role_for(Some(9001), Some("lxb-portal"), &running),
            Some(Role::Portal)
        );

        // Every one of these is a client this session started itself, which is
        // exactly the reasoning that used to let them all bind.
        assert_eq!(role_for(Some(9002), Some("steam"), &running), None);
        assert_eq!(role_for(Some(9003), Some("firefox"), &running), None);
        assert_eq!(role_for(Some(9004), Some("Xwayland"), &running), None);
    }

    /// The one this is all for: a program that calls itself by one of the
    /// session's own names is still not one of them.
    ///
    /// Nothing stops a process of this user copying itself to a file called
    /// `lxb-portal` and connecting — not a permission, not a policy, not this
    /// compositor. It is an ordinary file copy, and the session runs Valve's
    /// client, every game it starts, whatever the software hub installed and a
    /// browser, all as that same user. What the name used to buy was the
    /// interface that hides any window while leaving it running, closes
    /// anybody's, types, clicks, captures the screen and logs the user out.
    #[test]
    fn a_program_wearing_one_of_our_names_is_not_one_of_ours() {
        let running = session(4242, 9001);

        assert_eq!(role_for(Some(31337), Some("lxb-desktop"), &running), None);
        assert_eq!(role_for(Some(31338), Some("lxb-portal"), &running), None);
        // Including after the real one has gone and let its number go with it.
        let ended = Privileged::default();
        assert_eq!(role_for(Some(4242), Some("lxb-desktop"), &ended), None);
        assert_eq!(role_for(Some(9001), Some("lxb-portal"), &ended), None);
    }

    /// A connection whose peer cannot be read is nobody.
    ///
    /// It is the one place this could have been written the other way round —
    /// "if we cannot tell, allow it" — and that is a rule any client can meet
    /// by arranging not to be readable.
    #[test]
    fn a_peer_that_cannot_be_read_is_not_one_of_ours() {
        let running = session(4242, 9001);
        assert_eq!(role_for(None, None, &running), None);
        assert_eq!(role_for(Some(9006), None, &running), None);
        assert_eq!(role_for(None, Some("lxb-desktop"), &running), None);
    }

    /// A session that started nothing of its own recognises nothing, rather
    /// than everything.
    #[test]
    fn a_session_with_no_shell_of_its_own_recognises_nothing() {
        let nobody = Privileged::default();
        assert_eq!(role_for(Some(1), Some("lxb-desktop"), &nobody), None);
        assert_eq!(role_for(Some(2), Some("steam"), &nobody), None);
    }

    /// `--insecure-trust-program` is the old rule, and it is only ever what
    /// somebody typed. Named programs drive the session as a shell; everything
    /// else is refused exactly as before.
    #[test]
    fn a_name_is_trusted_only_when_an_operator_asked_for_it() {
        let asked = Privileged {
            shell: None,
            portal: None,
            by_name: vec!["lxb-desktop".to_string()],
        };

        assert_eq!(
            role_for(Some(7777), Some("lxb-desktop"), &asked),
            Some(Role::Shell)
        );
        assert_eq!(role_for(Some(7778), Some("lxb-portal"), &asked), None);
        assert_eq!(role_for(Some(7779), Some("steam"), &asked), None);
    }

    /// The portal may put a question to the shell and may do nothing else with
    /// the interface.
    ///
    /// The list is written the safe way round — what is allowed, rather than
    /// what is not — so a request added to this protocol later starts out of
    /// the portal's reach. These four are what it actually sends; the rest are
    /// a sample of what a portal taken over would otherwise reach for.
    #[test]
    fn the_portal_may_only_ask_the_shell_a_question() {
        assert!(portal_may_ask(&lxb_shell_v1::Request::Destroy));
        assert!(portal_may_ask(&lxb_shell_v1::Request::OfferKind {
            id: 1,
            name: String::new(),
            pattern: String::new(),
            matching: WEnum::Value(lxb_shell_v1::Matching::Glob),
        }));
        assert!(portal_may_ask(&lxb_shell_v1::Request::AskToPickFiles {
            id: 1,
            app_id: String::new(),
            purpose: WEnum::Value(lxb_shell_v1::Picking::OneFile),
            title: String::new(),
            accept: String::new(),
            name: String::new(),
            at: String::new(),
        }));
        assert!(portal_may_ask(&lxb_shell_v1::Request::AskToShare {
            id: 1,
            app_id: String::new(),
        }));

        // Hiding a window while it keeps running, which is the request that
        // makes a good enough hiding place that the shell uses it as one.
        assert!(!portal_may_ask(&lxb_shell_v1::Request::KeepOutOfSight {
            app_id: String::new(),
            hidden: 1,
        }));
        // Typing, clicking, and logging the user out.
        assert!(!portal_may_ask(&lxb_shell_v1::Request::KeyboardKey {
            key: 1,
            state: WEnum::Value(lxb_shell_v1::KeyState::Pressed),
        }));
        assert!(!portal_may_ask(&lxb_shell_v1::Request::MovePointer {
            dx: 0.0,
            dy: 0.0,
        }));
        assert!(!portal_may_ask(&lxb_shell_v1::Request::Quit));
        // And answering the questions it is the one that asks them.
        assert!(!portal_may_ask(&lxb_shell_v1::Request::AnswerShare {
            id: 1,
            output: None,
        }));
        assert!(!portal_may_ask(&lxb_shell_v1::Request::CloseForeground));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A display, for the launch records below.
    fn a_display(name: &str) -> Output {
        Output::new(
            name.to_string(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        )
    }

    /// A launch as the shell files it.
    fn a_launch(
        launch: u32,
        output: &Output,
        pid: Option<u32>,
        app_id: Option<&str>,
    ) -> LaunchRecord {
        LaunchRecord {
            launch,
            output: output.clone(),
            pid,
            app_id: app_id.map(str::to_string),
            filed: std::time::Instant::now(),
        }
    }

    /// A process tree of the test's own: a pid is descended from an ancestor
    /// when the ancestor's number divides it, which is enough to say "this
    /// window's process is one the shell started" without a machine under it.
    fn descends(pid: u32, ancestor: u32) -> bool {
        pid == ancestor || (ancestor != 0 && pid % ancestor == 0)
    }

    /// Two screens loading two things, whose windows arrive in the other
    /// order, open on the screens they were pressed on.
    ///
    /// This is the whole reason the records exist. There used to be one value
    /// for the session naming one launch, so *every* window that mapped went
    /// wherever that value pointed: the game started second held it while it
    /// loaded, and the game started first — slower, or simply unluckier —
    /// opened on the second screen beside it. Both games on one display, and
    /// neither on the display anybody pressed it on.
    #[test]
    fn two_launches_place_their_own_windows_however_they_arrive() {
        let (first, second) = (a_display("HDMI-A-1"), a_display("DP-1"));
        let mut launches = Vec::new();
        file_launch(&mut launches, a_launch(0, &first, Some(100), None));
        file_launch(&mut launches, a_launch(1, &second, Some(200), None));

        let now = std::time::Instant::now();
        // The newer launch's window arrives first.
        let placed = launch_for(&launches, now, Some(200), "", descends);
        assert_eq!(placed.map(|record| record.launch), Some(1));
        assert_eq!(placed.map(|record| &record.output), Some(&second));

        // And then the older one's, which is the half a single value got
        // wrong: it goes to the display it was started from, not to whichever
        // launch happens to be named now.
        let placed = launch_for(&launches, now, Some(100), "", descends);
        assert_eq!(placed.map(|record| record.launch), Some(0));
        assert_eq!(placed.map(|record| &record.output), Some(&first));
    }

    /// A window from a process the launch only started indirectly is still
    /// that launch's.
    ///
    /// The ordinary case rather than the exotic one: a desktop entry runs a
    /// shell script, a game runs under a container and a runtime and a
    /// compatibility tool, a launcher execs the thing it launched.
    #[test]
    fn a_windows_launch_is_found_up_the_process_chain() {
        let display = a_display("HDMI-A-1");
        let launches = vec![a_launch(0, &display, Some(7), None)];
        let now = std::time::Instant::now();
        assert!(launch_for(&launches, now, Some(49), "", descends).is_some());
        assert!(launch_for(&launches, now, Some(50), "", descends).is_none());
    }

    /// A game handed to Valve's client has no process of the shell's, and is
    /// recognised by the class its window carries instead.
    #[test]
    fn a_game_is_placed_by_the_class_its_window_carries() {
        let display = a_display("DP-1");
        let launches = vec![a_launch(0, &display, None, Some("steam_app_1145360"))];
        let now = std::time::Instant::now();
        assert!(launch_for(&launches, now, None, "steam_app_1145360", descends).is_some());
        // Spelled the other way by whichever toolkit set it, and still the
        // same game.
        assert!(launch_for(&launches, now, None, "Steam_App_1145360", descends).is_some());
        assert!(launch_for(&launches, now, None, "steam_app_367520", descends).is_none());
    }

    /// A window that answers to no record is left to the session's own answer,
    /// which is what places every window on a session that files none.
    #[test]
    fn a_window_that_matches_nothing_is_placed_by_nothing() {
        let display = a_display("HDMI-A-1");
        let launches = vec![a_launch(0, &display, Some(100), Some("steam_app_1"))];
        let now = std::time::Instant::now();
        assert!(launch_for(&launches, now, Some(101), "firefox", descends).is_none());
        // A window that named itself nothing must not be claimed by a record
        // that named nothing either.
        let anonymous = vec![a_launch(0, &display, None, None)];
        assert!(launch_for(&anonymous, now, Some(101), "", descends).is_none());
    }

    /// A second press on one screen is that screen's record changing, not a
    /// second record.
    #[test]
    fn a_screen_holds_one_launch_record() {
        let (first, second) = (a_display("HDMI-A-1"), a_display("DP-1"));
        let mut launches = Vec::new();
        file_launch(&mut launches, a_launch(0, &first, Some(100), None));
        file_launch(&mut launches, a_launch(0, &second, Some(200), None));
        assert_eq!(launches.len(), 1);
        let now = std::time::Instant::now();
        assert!(launch_for(&launches, now, Some(100), "", descends).is_none());
        assert_eq!(
            launch_for(&launches, now, Some(200), "", descends).map(|record| &record.output),
            Some(&second)
        );
    }

    /// A record nobody came back for is dropped rather than kept for the rest
    /// of the session.
    #[test]
    fn a_launch_nobody_took_back_goes_stale() {
        let display = a_display("HDMI-A-1");
        let mut old = a_launch(0, &display, Some(100), None);
        old.filed = std::time::Instant::now() - UNTIL_A_LAUNCH_IS_STALE * 2;
        let launches = vec![old.clone()];
        let now = std::time::Instant::now();
        assert!(launch_for(&launches, now, Some(100), "", descends).is_none());

        // And the next press is what sweeps it out of the list.
        let mut launches = vec![old];
        file_launch(&mut launches, a_launch(1, &display, Some(200), None));
        assert_eq!(launches.len(), 1);
    }

    /// The two answers the shell gives are two, and the one about the person
    /// wins.
    ///
    /// The failure this is about: a game loading on the second screen holds the
    /// launch answer for the whole of its loading time — which is right, and is
    /// what opens it on the screen it was pressed on — and read as "where the
    /// user is" it walked the keyboard and the X screen's active window off to
    /// a display nobody was looking at.
    #[test]
    fn the_driven_display_is_not_the_launch_display() {
        let (driving, loading) = (a_display("HDMI-A-1"), a_display("DP-1"));
        assert_eq!(
            the_display_being_driven(Some(&driving), Some(&loading)),
            Some(&driving)
        );
        // A shell too old to say has one value for both, and it is read for
        // both — which is exactly what this compositor did before the two came
        // apart.
        assert_eq!(
            the_display_being_driven(None, Some(&loading)),
            Some(&loading)
        );
        assert_eq!(the_display_being_driven(None, None), None);
    }

    /// The process chain is walked on the machine the same way the rule above
    /// walks a made-up one.
    #[test]
    fn a_child_of_this_process_is_descended_from_it() {
        let ours = std::process::id();
        assert!(descends_from(ours, ours));
        let Ok(mut child) = std::process::Command::new("sleep").arg("30").spawn() else {
            // No `sleep` on the machine running the tests. The rule above is
            // what this checks; this is the half that needs a process table.
            return;
        };
        assert_eq!(parent_of(child.id()), Some(ours));
        assert!(descends_from(child.id(), ours));
        assert!(!descends_from(ours, child.id()));
        let _ = child.kill();
        let _ = child.wait();
    }

    /// The variant is *removed* when there is none, not set empty.
    ///
    /// An empty variant is what "the layout's own arrangement" means, and a
    /// stale one left behind from a previous choice would be a different
    /// keyboard: `pl` with `dvorak` still exported is Polish Dvorak.
    #[test]
    fn the_exported_keyboard_carries_no_stale_variant() {
        // Read before, put back after: this test touches only the two
        // variables it is about.
        let (layout, variant) = (
            std::env::var("XKB_DEFAULT_LAYOUT").ok(),
            std::env::var("XKB_DEFAULT_VARIANT").ok(),
        );
        export_keyboard_environment("pl", "dvorak");
        assert_eq!(std::env::var("XKB_DEFAULT_LAYOUT").as_deref(), Ok("pl"));
        assert_eq!(
            std::env::var("XKB_DEFAULT_VARIANT").as_deref(),
            Ok("dvorak")
        );
        export_keyboard_environment("de", "");
        assert_eq!(std::env::var("XKB_DEFAULT_LAYOUT").as_deref(), Ok("de"));
        assert!(std::env::var("XKB_DEFAULT_VARIANT").is_err());
        // SAFETY: single-threaded test; nothing else reads these two.
        unsafe {
            match layout {
                Some(layout) => std::env::set_var("XKB_DEFAULT_LAYOUT", layout),
                None => std::env::remove_var("XKB_DEFAULT_LAYOUT"),
            }
            match variant {
                Some(variant) => std::env::set_var("XKB_DEFAULT_VARIANT", variant),
                None => std::env::remove_var("XKB_DEFAULT_VARIANT"),
            }
        }
    }

    fn output(name: &str) -> Output {
        Output::new(
            name.to_string(),
            smithay::output::PhysicalProperties {
                size: (0, 0).into(),
                subpixel: smithay::output::Subpixel::Unknown,
                make: "test".into(),
                model: "test".into(),
            },
        )
    }

    /// A press is told to the shell when it lands on a display other than the
    /// one the last press was told about, and only then: a hand clicking about
    /// inside a game must not wake the shell once per click, and the display
    /// that hand is on has not changed.
    #[test]
    fn only_a_press_on_another_display_is_told_to_the_shell() {
        let first = output("A");
        let second = output("B");

        // Nothing remembered: the first press of the session, and every press
        // after the shell has named a launch display and this was forgotten, so
        // that a press back on the display it moved away from is news again.
        assert!(press_is_news(None, &first));

        assert!(!press_is_news(Some(&first), &first));
        assert!(press_is_news(Some(&first), &second));

        // The display, not its name: a display unplugged and plugged back in is
        // a new `Output` with the old name, and a press on it is news.
        assert!(press_is_news(Some(&first), &output("A")));
    }
}
