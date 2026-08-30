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
    ) -> anyhow::Result<crate::capture::Shot> {
        match self {
            Backend::Winit(b) => b.picture_behind(lxb, output, side, size),
            Backend::X11(b) => b.picture_behind(lxb, output, side, size),
            Backend::Udev(b) => b.picture_behind(lxb, output, side, size),
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
}
