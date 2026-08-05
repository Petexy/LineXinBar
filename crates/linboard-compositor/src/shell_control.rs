//! The compositor half of `linboard_shell_v1`.
//!
//! Two things a console-style session needs cannot be expressed in
//! layer-shell. The first is the guide button: while a fullscreen application
//! owns the keyboard the shell receives no key events at all, which is exactly
//! the moment the user needs a way back out of it, so the compositor has to
//! route that one binding itself. The second is closing that application, which
//! only the compositor can ask for politely.

use linboard_protocol::server::linboard_shell_v1::{self, LinboardShellV1};
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::reexports::wayland_server::backend::{ClientId, GlobalId};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::wayland::compositor::with_states;
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::shell::xdg::XdgToplevelSurfaceData;

use crate::input::window_accepts_keyboard_focus;
use crate::state::LinboardState;

/// One window as the overview describes it: the id the shell can activate it
/// by, its title, and its logical size (for aspect-fitting the card frame).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverviewEntry {
    pub id: u32,
    pub title: String,
    pub width: u32,
    pub height: u32,
}

/// Tracks every shell bound to the protocol, plus the state they were last
/// told about.
#[derive(Debug)]
pub struct ShellControlState {
    #[allow(dead_code)]
    global: GlobalId,
    instances: Vec<LinboardShellV1>,
    /// Shells bound so recently that they may not have their `wl_output`s
    /// yet. Per-display events name an output, so one sent before the client
    /// has bound any reaches nobody — and the caches below would then record
    /// it as delivered and never send it again, leaving a shell that started
    /// while applications were already running convinced nothing is. They
    /// are resent, in full, until one lands.
    awaiting_outputs: Vec<LinboardShellV1>,
    /// Last session-wide title broadcast, so an unchanged foreground sends
    /// nothing. Only reaches clients older than version 3.
    foreground: String,
    /// Last title broadcast per display, likewise. Rebuilt from the live
    /// outputs on every refresh, so a display going away drops out of it.
    output_foreground: Vec<(Output, String)>,
    /// Last window list broadcast per display, likewise.
    output_windows: Vec<(Output, Vec<OverviewEntry>)>,
    /// Display the shell says the user is on, from `set_launch_output`.
    launch_output: Option<Output>,
}

/// First version that reports the foreground application per display. Below
/// it, a shell only learns about the session as a whole.
const PER_OUTPUT_SINCE: u32 = 3;

/// First version with the window overview: per-display window lists, and the
/// requests to enter it and to activate a window from it.
const OVERVIEW_SINCE: u32 = 4;

/// The version advertised, and so the highest a shell can bind: adds
/// `kill_window`. Every request below it is still served, so an older shell
/// keeps working.
const CURRENT_VERSION: u32 = 5;

impl ShellControlState {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<LinboardShellV1, ()> + 'static,
    {
        Self {
            global: display.create_global::<D, LinboardShellV1, _>(CURRENT_VERSION, ()),
            instances: Vec::new(),
            awaiting_outputs: Vec::new(),
            foreground: String::new(),
            output_foreground: Vec::new(),
            output_windows: Vec::new(),
            launch_output: None,
        }
    }

    /// Whether a shell is listening. Used only to explain an inert binding.
    pub fn has_shell(&self) -> bool {
        !self.instances.is_empty()
    }

    pub fn launch_output(&self) -> Option<&Output> {
        self.launch_output.as_ref()
    }

    fn send_guide(&self) {
        for instance in &self.instances {
            instance.guide();
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

    /// Bring a newly bound shell up to date, since the broadcasts above only
    /// carry changes.
    ///
    /// Returns whether anything actually reached it: a client with no
    /// `wl_output` bound yet can be told nothing per-display.
    fn send_current(&self, shell: &LinboardShellV1) -> bool {
        if shell.version() < PER_OUTPUT_SINCE {
            shell.foreground(self.foreground.clone());
            return true;
        }
        let mut sent = false;
        for (output, title) in &self.output_foreground {
            sent |= send_output_foreground(shell, output, title);
        }
        for (output, windows) in &self.output_windows {
            sent |= send_output_windows(shell, output, windows);
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
fn send_output_foreground(shell: &LinboardShellV1, output: &Output, title: &str) -> bool {
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

/// Send one display's whole window list, ending with the done event that
/// makes the batch replace whatever the shell knew before.
fn send_output_windows(
    shell: &LinboardShellV1,
    output: &Output,
    windows: &[OverviewEntry],
) -> bool {
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
        }
        shell.output_windows_done(&wl_output);
        sent = true;
    }
    sent
}

impl LinboardState {
    /// Tell the shell the user asked for its overlay.
    pub fn open_guide(&mut self) {
        if !self.linboard.shell_control.has_shell() {
            tracing::debug!("guide binding pressed but no shell has bound linboard_shell_v1");
            return;
        }
        self.linboard.shell_control.send_guide();
    }

    /// Publish the foreground application's title, if it changed — both for
    /// the session as a whole and for each display.
    ///
    /// The *topmost* window rather than the focused one: while the overlay is
    /// up the shell itself holds focus, and the application it is offering to
    /// close must not read as having disappeared.
    pub fn refresh_foreground(&mut self) {
        if !self.linboard.shell_control.has_shell() {
            return;
        }

        let title = self
            .topmost_application(None)
            .map(|window| window_title(&window))
            .unwrap_or_default();
        self.linboard.shell_control.broadcast_foreground(title);

        let outputs: Vec<Output> = self.linboard.space.outputs().cloned().collect();
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
        self.linboard
            .shell_control
            .broadcast_output_foreground(per_output);

        // The overview's window lists ride the same refresh: they are diffed
        // per display, so an unchanged desktop sends nothing.
        let per_output_windows = outputs
            .into_iter()
            .map(|output| {
                let windows = crate::render::overview_windows(&self.linboard.space, &output)
                    .into_iter()
                    .map(|window| {
                        let size = self
                            .linboard
                            .space
                            .element_geometry(&window)
                            .map(|geometry| geometry.size)
                            .unwrap_or_default();
                        OverviewEntry {
                            id: crate::overview::window_id(&window),
                            title: window_title(&window),
                            width: size.w.max(0) as u32,
                            height: size.h.max(0) as u32,
                        }
                    })
                    .collect();
                (output, windows)
            })
            .collect();
        self.linboard
            .shell_control
            .broadcast_output_windows(per_output_windows);

        // Those broadcasts only carry changes, so a shell whose outputs were
        // not ready when it bound would otherwise wait for the desktop to
        // change before learning what is on it.
        self.linboard.shell_control.catch_up_new_shells();
    }

    /// The window an overview id names, if it is still mapped.
    fn window_by_overview_id(&self, id: u32) -> Option<Window> {
        self.linboard
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

    /// End the process behind one window, without asking it.
    ///
    /// The shell's Close is a console's power-off for one application, not a
    /// file menu's Quit: it has to work on something that has stopped
    /// answering, which is exactly when `send_close` does nothing.
    ///
    /// The pid comes from the window's own client, so this reaches the
    /// application and not the compositor's other children. An X11 window is
    /// the interesting case: its Wayland client is Xwayland itself, and
    /// killing *that* would take the whole X session with it, so the pid is
    /// read from the window instead.
    pub fn kill_window(&mut self, window: &Window) {
        let pid = if let Some(surface) = window.x11_surface() {
            surface
                .pid()
                .or_else(|| surface.get_client_pid().ok())
                .map(|pid| pid as i32)
        } else {
            window
                .wl_surface()
                .and_then(|surface| surface.client())
                .and_then(|client| client.get_credentials(&self.linboard.display_handle).ok())
                .map(|credentials| credentials.pid)
        };

        let Some(pid) = pid.filter(|pid| *pid > 1) else {
            // Nothing to signal — a remote or forwarded client. Falling back
            // to the polite request is still better than doing nothing.
            tracing::warn!("no process to kill for this window; asking it to close instead");
            self.request_window_close(window);
            return;
        };

        tracing::info!(pid, title = window_title(window), "killing application");
        // SAFETY: `kill` with a validated positive pid; the worst outcome of a
        // recycled pid is ESRCH, which is ignored below.
        if unsafe { libc::kill(pid, libc::SIGKILL) } != 0 {
            let err = std::io::Error::last_os_error();
            tracing::warn!(?err, pid, "could not kill the application");
        }
    }

    /// Topmost window that takes keyboard focus, across the session or on one
    /// display.
    fn topmost_application(&self, output: Option<&Output>) -> Option<Window> {
        self.linboard
            .space
            .elements()
            .rev()
            .find(|window| {
                window_accepts_keyboard_focus(window)
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
        let geometry = self.linboard.space.element_geometry(window)?;
        self.linboard
            .space
            .outputs()
            .filter_map(|output| {
                let area = self.linboard.space.output_geometry(output)?;
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
        let wanted = self.linboard.shell_control.launch_output()?;
        self.linboard
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

impl GlobalDispatch<LinboardShellV1, ()> for LinboardState {
    fn bind(
        state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<LinboardShellV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let shell = data_init.init(resource, ());
        // A shell that binds late still needs to know what is on screen, and
        // the broadcasts only carry changes. This early it will usually have
        // no `wl_output` bound yet, so it is queued for another try.
        if !state.linboard.shell_control.send_current(&shell) {
            state
                .linboard
                .shell_control
                .awaiting_outputs
                .push(shell.clone());
        }
        state.linboard.shell_control.instances.push(shell);
        tracing::info!("session shell bound linboard_shell_v1");

        // The first instance turns foreground tracking on; publish the current
        // window straight away rather than waiting for the next change.
        state.refresh_foreground();
    }
}

impl Dispatch<LinboardShellV1, ()> for LinboardState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &LinboardShellV1,
        request: linboard_shell_v1::Request,
        _data: &(),
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            linboard_shell_v1::Request::CloseForeground => state.close_foreground_window(None),
            linboard_shell_v1::Request::CloseOutputForeground { output } => {
                match Output::from_resource(&output) {
                    Some(output) => state.close_foreground_window(Some(&output)),
                    // The display went away between the shell drawing its menu
                    // and the user choosing from it.
                    None => tracing::debug!("close requested for a display that is gone"),
                }
            }
            linboard_shell_v1::Request::SetLaunchOutput { output } => {
                let output = Output::from_resource(&output);
                tracing::debug!(
                    display = output.as_ref().map(|o| o.name()).unwrap_or_default(),
                    "shell chose the display to launch applications on"
                );
                state.linboard.shell_control.launch_output = output;
            }
            linboard_shell_v1::Request::SetOutputOverview { output, enabled } => {
                match Output::from_resource(&output) {
                    Some(output) => {
                        tracing::debug!(
                            display = %output.name(),
                            enabled,
                            "shell toggled the window overview"
                        );
                        state.linboard.overview.set(
                            &output,
                            enabled != 0,
                            std::time::Instant::now(),
                        );
                    }
                    None => tracing::debug!("overview toggled on a display that is gone"),
                }
            }
            linboard_shell_v1::Request::SetOverviewSelection { output, index } => {
                if let Some(output) = Output::from_resource(&output) {
                    state
                        .linboard
                        .overview
                        .set_selection(&output, index as usize);
                }
            }
            linboard_shell_v1::Request::ActivateWindow { id } => {
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
            linboard_shell_v1::Request::KillWindow { id } => {
                match state.window_by_overview_id(id) {
                    Some(window) => state.kill_window(&window),
                    None => tracing::debug!(id, "kill of a window that is gone"),
                }
            }
            linboard_shell_v1::Request::Quit => {
                tracing::info!("session shell requested shutdown");
                state.linboard.running = false;
                state.linboard.loop_signal.stop();
            }
            linboard_shell_v1::Request::Destroy => {}
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &LinboardShellV1, _data: &()) {
        state
            .linboard
            .shell_control
            .instances
            .retain(|instance| instance != resource);

        // With no shell left there is nobody to close the overview, and a
        // desktop whose windows are all shrunk into cards is unusable. A
        // shell that crashes or is restarted must not leave one behind.
        if !state.linboard.shell_control.has_shell() {
            state.linboard.overview.close_all(std::time::Instant::now());
        }
    }
}
