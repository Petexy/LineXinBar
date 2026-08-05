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
}

impl crate::state::LinboardState {
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
}
