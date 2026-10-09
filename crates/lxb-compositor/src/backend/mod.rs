//! Backends.
//!
//! * [`winit`] — a single nested window inside an existing Wayland (or X11)
//!   session. This is the debugging backend.
//! * [`x11`] — nested as well, but able to open *several* windows, each acting
//!   as a virtual output. Lets the multi-display code be exercised without
//!   owning several physical monitors.
//! * [`udev`] — the real thing: DRM/KMS, libinput, libseat, one CRTC per
//!   connected display.

pub mod udev;
pub mod winit;
pub mod x11;

use smithay::backend::allocator::dmabuf::Dmabuf;

/// The active backend.
pub enum Backend {
    Winit(Box<winit::WinitBackend>),
    X11(Box<x11::X11Backend>),
    Udev(Box<udev::UdevBackend>),
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("import failed: {0}")]
    Failed(String),
}

/// Whether switching one display off or on is this session's to do, and if
/// not, why not. The `connector_switching` enum of `lxb_shell_v1`.
///
/// Reported per display and before anybody presses anything, so that a page
/// never offers a press it would have to refuse. The three refusals are
/// genuinely different and a shell has a different thing to say about each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switching {
    /// It can be switched either way.
    Allowed,
    /// It is the only display left on, so it stays on. There has to be
    /// somewhere to draw the page that would switch it back.
    LastOneOn,
    /// A hand-written config file answers for this display itself — see
    /// [`crate::config::Config::enabled_is_pinned`]. A press here would work
    /// until the next login and then be overruled, which is worse than not
    /// offering it.
    SetElsewhere,
    /// This session cannot switch a display at all: the one nested window a
    /// winit session is, which has nothing to switch off and no way back.
    NotPossible,
}

impl Switching {
    /// What goes on the wire.
    pub fn as_protocol(self) -> lxb_protocol::server::lxb_shell_v1::ConnectorSwitching {
        use lxb_protocol::server::lxb_shell_v1::ConnectorSwitching as Wire;
        match self {
            Self::Allowed => Wire::Allowed,
            Self::LastOneOn => Wire::LastOneOn,
            Self::SetElsewhere => Wire::SetElsewhere,
            Self::NotPossible => Wire::NotPossible,
        }
    }
}

/// One display this session has, whether or not anything is drawn on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connector {
    /// The connector's own name — `DP-1`, `HDMI-A-1`. The same string
    /// `wl_output.name` gives while the display is lit, and the only name it
    /// has at all while it is not.
    pub name: String,
    /// Whether it is being driven now.
    pub lit: bool,
    /// Whether that is the shell's to change.
    pub switching: Switching,
}

/// What came of asking for a display to be switched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Switched {
    /// It was switched, and the session has changed.
    Done,
    /// It was already that way; nothing happened and nothing is wrong.
    Already,
    /// It was not switched. Never reported to the client as such — the
    /// connector event says what the session is actually doing, which is the
    /// one answer a shell must not present as success.
    Refused,
}

impl Backend {
    /// Seat name to advertise. Nested backends make one up; udev uses the real
    /// libseat seat.
    pub fn seat_name(&self) -> String {
        match self {
            Backend::Winit(_) => "winit".to_string(),
            Backend::X11(_) => "x11".to_string(),
            Backend::Udev(b) => b.seat_name(),
        }
    }

    /// Switch to a Linux VT. Only meaningful for the udev backend.
    pub fn switch_vt(&mut self, vt: i32) {
        match self {
            Backend::Udev(b) => b.switch_vt(vt),
            _ => tracing::debug!(vt, "VT switching is not available when nested"),
        }
    }

    /// Hand the pointing-device settings to the devices already open.
    ///
    /// Only the udev backend has any: a nested session's pointer belongs to the
    /// compositor above it, which is where its speed and its scroll direction
    /// are set, and there is nothing here to apply them to. Silent rather than
    /// warned about — a nested run is a development session and would say this
    /// on every press.
    pub fn apply_input_settings(&mut self, config: &crate::config::Input) {
        if let Backend::Udev(b) = self {
            b.apply_input_settings(config);
        }
    }

    /// Draw this backend's own cursor at this many logical pixels.
    ///
    /// Every backend draws one, unlike the settings above: the pointer is a
    /// picture this compositor composites, whatever it is reaching a screen
    /// through.
    pub fn set_cursor_size(&mut self, size: u32) {
        match self {
            Backend::Winit(b) => b.set_cursor_size(size),
            Backend::X11(b) => b.set_cursor_size(size),
            Backend::Udev(b) => b.set_cursor_size(size),
        }
    }

    /// Validate a client dmabuf against the backend's renderer.
    pub fn import_dmabuf(&mut self, dmabuf: &Dmabuf) -> Result<(), ImportError> {
        match self {
            Backend::Winit(b) => b.import_dmabuf(dmabuf),
            Backend::X11(b) => b.import_dmabuf(dmabuf),
            Backend::Udev(b) => b.import_dmabuf(dmabuf),
        }
    }

    /// Photograph one window with this backend's renderer.
    ///
    /// Every backend can: the picture is drawn into an offscreen buffer rather
    /// than onto a screen, so it needs a renderer and nothing else about how
    /// this session reaches its displays.
    pub fn capture_window(
        &mut self,
        window: &smithay::desktop::Window,
        scale: f64,
    ) -> anyhow::Result<crate::capture::Shot> {
        match self {
            Backend::Winit(b) => b.capture_window(window, scale),
            Backend::X11(b) => b.capture_window(window, scale),
            Backend::Udev(b) => b.capture_window(window, scale),
        }
    }

    /// Photograph one whole display with this backend's renderer.
    ///
    /// The compositor state comes in beside the backend rather than being
    /// reached through it, because the picture is the composite: everything on
    /// that display, which is what [`crate::render::output_elements`] assembles
    /// and what only the session state knows. The two halves are separate
    /// fields of [`crate::state::LxbState`] for exactly this reason — a render
    /// pass borrows both at once, and a screenshot is a render pass.
    pub fn capture_output(
        &mut self,
        lxb: &crate::state::Lxb,
        output: &smithay::output::Output,
    ) -> anyhow::Result<crate::capture::Shot> {
        match self {
            Backend::Winit(b) => b.capture_output(lxb, output),
            Backend::X11(b) => b.capture_output(lxb, output),
            Backend::Udev(b) => b.capture_output(lxb, output),
        }
    }

    /// Draw, small, what this backend is compositing on one side of the shell's
    /// own surfaces — so a pane of the shell's glass can refract it.
    ///
    /// Here for the reason [`Backend::capture_output`] is here: it is a render
    /// pass into an offscreen buffer, which needs a renderer and the session
    /// state and nothing about how this session reaches its displays. See
    /// [`crate::capture::behind`].
    pub fn picture_behind(
        &mut self,
        lxb: &crate::state::Lxb,
        output: &smithay::output::Output,
        side: crate::capture::Side,
        size: smithay::utils::Size<i32, smithay::utils::Physical>,
        at: std::time::Instant,
    ) -> anyhow::Result<crate::capture::Shot> {
        match self {
            Backend::Winit(b) => b.picture_behind(lxb, output, side, size, at),
            Backend::X11(b) => b.picture_behind(lxb, output, side, size, at),
            Backend::Udev(b) => b.picture_behind(lxb, output, side, size, at),
        }
    }
}

impl crate::state::LxbState {
    /// Tell the active backend that something on screen changed.
    ///
    /// The nested backends repaint on a fixed timer and ignore this; the DRM
    /// backend is vblank driven, so it needs the nudge to show a change on the
    /// next retrace instead of waiting for its idle poll.
    ///
    /// This lives here, rather than as a backend check at each call site, so
    /// that protocol handlers never have to name a backend. It takes the whole
    /// state because scheduling a DRM frame needs both halves of it.
    pub fn queue_redraw(&mut self) {
        if let Backend::Udev(_) = self.backend {
            udev::queue_redraw_all(self);
        }
    }

    /// What one display can be driven at.
    ///
    /// Only the DRM backend has an answer: a mode list belongs to a connector,
    /// and a nested session owns none — it is the size of the window the
    /// parent compositor gave it, which is that compositor's business and not
    /// something a client of this one may change. Reporting nothing there is
    /// the truth, and it is what leaves the shell's page saying so.
    pub fn output_modes(
        &self,
        output: &smithay::output::Output,
    ) -> Vec<crate::outputs::DisplayMode> {
        match self.backend {
            Backend::Udev(_) => udev::output_modes(self, output),
            _ => Vec::new(),
        }
    }

    /// Drive one display at a different mode. `true` when it took.
    pub fn set_output_mode(
        &mut self,
        output: &smithay::output::Output,
        want: crate::config::ModeRequest,
    ) -> bool {
        match self.backend {
            Backend::Udev(_) => udev::set_output_mode(self, output, want),
            _ => {
                tracing::debug!("a nested session is the size of its window; mode unchanged");
                false
            }
        }
    }

    /// How one display's picture is turned, where the turn is this
    /// compositor's to make.
    ///
    /// Turning is renderer-side rather than anything a connector has to
    /// support, so unlike a mode list it is not the DRM backend's alone: an
    /// x11 window standing in for a display is composited into just as much as
    /// a panel is, and is turned the same way.
    ///
    /// `None` for winit, which is the one output whose transform is not the
    /// user's answer to anything: the backend gives it a flip of its own to
    /// compensate for the way it draws, and a turn assigned over the top of
    /// that would land the picture upside down at "landscape". Nothing is
    /// reported for it, so the shell leaves it off the page rather than
    /// offering a setting that would come out wrong.
    pub fn output_transform(
        &self,
        output: &smithay::output::Output,
    ) -> Option<smithay::utils::Transform> {
        match self.backend {
            Backend::Winit(_) => None,
            _ => Some(output.current_transform()),
        }
    }

    /// Turn one display's picture. `true` when anything changed.
    pub fn set_output_transform(
        &mut self,
        output: &smithay::output::Output,
        transform: smithay::utils::Transform,
    ) -> bool {
        if self.output_transform(output).is_none() {
            tracing::debug!(
                output = %output.name(),
                "this display's orientation is not this compositor's to set"
            );
            return false;
        }
        let config = self.lxb.config.clone();
        let turned =
            self.lxb
                .outputs
                .set_transform(&mut self.lxb.space, output, transform, &config);
        if turned {
            // The turn is drawn rather than scanned out, so nothing reaches the
            // screen until the display draws again — which on an idle session
            // is a retrace away and on a covered one may never come.
            self.queue_redraw();
        }
        turned
    }

    /// Every display this session has, lit or dark, in the order the backend
    /// knows them.
    ///
    /// The list the shell's Screens-in-use page is built from, and the only
    /// list in this compositor that includes a display nobody is driving. A
    /// lit display appears here *and* as a `wl_output`; a dark one appears
    /// only here, because there is nothing to drive and so nothing for a
    /// client to bind.
    pub fn connectors(&self) -> Vec<Connector> {
        let mut found = match &self.backend {
            Backend::Udev(_) => udev::connectors(self),
            Backend::X11(_) => x11::connectors(self),
            // One window, no second one to fall back to, and no path that
            // could take it away and give it back. Reported as a display that
            // exists and cannot be switched, rather than left out: a page that
            // said nothing here would read as a session with no displays.
            Backend::Winit(_) => self
                .lxb
                .space
                .outputs()
                .map(|output| Connector {
                    name: output.name(),
                    lit: true,
                    switching: Switching::NotPossible,
                })
                .collect(),
        };
        // The floor, applied once and in one place rather than in each backend:
        // the last display on cannot be switched off, whatever the backend
        // would otherwise allow. Counted over this list rather than over the
        // space, so that it agrees exactly with what is reported — a display
        // the backend has only just taken down is already dark here.
        let lit = found.iter().filter(|entry| entry.lit).count();
        if lit <= 1 {
            for entry in &mut found {
                if entry.lit && entry.switching == Switching::Allowed {
                    entry.switching = Switching::LastOneOn;
                }
            }
        }
        found
    }

    /// Switch one display off, or back on, naming it as the shell names it.
    ///
    /// The floor lives here rather than in any one backend, and is taken again
    /// at the moment the request runs: the shell decided against the session it
    /// could see, and by now the other display's cable may already be out.
    /// Requests and hot-plug events share the one event loop thread, so the
    /// count taken here is the only one that cannot be stale.
    pub fn set_connector_enabled(&mut self, name: &str, on: bool) -> Switched {
        let known = self.connectors();
        let Some(entry) = known.iter().find(|entry| entry.name == name) else {
            tracing::debug!(display = name, "no such display to switch");
            return Switched::Refused;
        };
        if entry.lit == on {
            return Switched::Already;
        }
        match entry.switching {
            Switching::Allowed => {}
            refusal => {
                tracing::info!(
                    display = name,
                    ?refusal,
                    "this display is not this session's to switch"
                );
                return Switched::Refused;
            }
        }

        let switched = match &self.backend {
            Backend::Udev(_) => udev::set_connector_enabled(self, name, on),
            Backend::X11(_) => x11::set_connector_enabled(self, name, on),
            Backend::Winit(_) => false,
        };
        if !switched {
            return Switched::Refused;
        }

        // The session's own answer, which is what every later hot-plug, device
        // rescan and return from a text console reads — the config is loaded
        // once and never written, so without this the display would be lit
        // again by the next thing that asked.
        self.lxb.outputs.switch(name, on);
        // And the next session's. Only on a change that went through: a
        // request this compositor would not carry out is never written down as
        // if it had been. Never the `"*"` entry either — `output_for` falls
        // back to it, so one answer there would be an answer about every
        // display on the machine — but a wildcard is pinned by the config and
        // was refused above, so there is nothing further to check here.
        crate::remembered::remember(name, |entry| entry.enabled = Some(on));

        tracing::info!(display = name, on, "a display was switched");
        self.refresh_connectors();
        Switched::Done
    }
}
