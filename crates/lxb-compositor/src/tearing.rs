//! `wp_tearing_control_v1`: letting a client say it would rather see the frame
//! now than see it whole.
//!
//! A compositor normally swaps what the screen is scanning out during the
//! vertical blank, so a frame is never half of one picture and half of
//! another. That costs latency: a frame finished a moment after the blank
//! waits a whole refresh before anybody sees it. A game asking for
//! `VK_PRESENT_MODE_IMMEDIATE_KHR` is asking to skip that wait and accept the
//! seam across the screen that comes with it, which for a fighting game — a
//! genre decided by single frames — is a trade its players make deliberately.
//!
//! Wayland has no way to ask for that without this protocol, so its absence is
//! not neutral: Mesa only offers a client the immediate present mode when the
//! compositor carries this global. Advertising it without being able to tear
//! would therefore be worse than staying quiet — the client would render
//! uncapped and still be shown at the retrace, paying the cost and getting
//! none of the benefit. LineXinBar can tear, through a patched smithay; see
//! `DrmSurface::set_tearing` and the note in the workspace manifest.
//!
//! ## What is honoured
//!
//! The hint belongs to a surface, and is double-buffered like everything else
//! about one: it takes effect with the commit that carries it, so a client can
//! turn tearing on and off between frames without a protocol round trip.
//!
//! Only the application in front of a display tears, and only where it is the
//! whole picture. Tearing is a property of the scanout, not of a window: there
//! is one page flip for the display and it either waits for the blank or does
//! not, so a torn frame under a shell drawn over it would tear the shell too.
//! The guide, the overlay and the start screen are all reasons to stop, and
//! [`crate::render`] decides that per frame rather than once.
//!
//! ## What is not promised
//!
//! Nothing. The protocol calls it a hint and means it: the driver refuses an
//! immediate flip whenever the commit changes more than the primary plane's
//! address, and a refused flip is retried at the retrace rather than dropped.
//! A client that asks for tearing on a machine that cannot do it sees the
//! behaviour it would have seen anyway.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use smithay::reexports::wayland_protocols::wp::tearing_control::v1::server::{
    wp_tearing_control_manager_v1::{self, WpTearingControlManagerV1},
    wp_tearing_control_v1::{self, PresentationHint, WpTearingControlV1},
};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::IsAlive;
use smithay::wayland::compositor::{with_states, Cacheable};

/// The hint a surface is carrying, as its client last committed it.
///
/// `Cacheable` rather than a plain flag, because the protocol makes this part
/// of the surface's double-buffered state: the value a client sets applies to
/// the frame it commits next, not to the one already on screen.
#[derive(Debug, Default, Clone, Copy)]
pub struct TearingCachedState {
    /// Whether this surface asked to be shown without waiting for the retrace.
    pub tearing: bool,
}

impl Cacheable for TearingCachedState {
    fn commit(&mut self, _dh: &DisplayHandle) -> Self {
        *self
    }

    fn merge_into(self, into: &mut Self, _dh: &DisplayHandle) {
        *into = self;
    }
}

/// Whether `surface` last committed a request to be shown immediately.
pub fn surface_wants_tearing(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        states
            .cached_state
            .get::<TearingCachedState>()
            .current()
            .tearing
    })
}

/// Per-`wp_tearing_control_v1` data: the surface it speaks for, and whether it
/// is still alive.
///
/// The surface is held weakly in effect — through the resource's own liveness
/// check — because a client may destroy the surface first, and a hint set on a
/// dead surface is not an error, merely nothing.
#[derive(Debug)]
pub struct TearingControlData {
    surface: WlSurface,
}

/// The global.
#[derive(Debug)]
pub struct TearingControlState {
    /// Kept so the global outlives the state that created it.
    _global: smithay::reexports::wayland_server::backend::GlobalId,
}

impl TearingControlState {
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<WpTearingControlManagerV1, ()>
            + Dispatch<WpTearingControlManagerV1, ()>
            + Dispatch<WpTearingControlV1, Arc<TearingControlData>>
            + 'static,
    {
        Self {
            _global: display.create_global::<D, WpTearingControlManagerV1, _>(1, ()),
        }
    }
}

impl<D> GlobalDispatch<WpTearingControlManagerV1, (), D> for TearingControlState
where
    D: GlobalDispatch<WpTearingControlManagerV1, ()> + Dispatch<WpTearingControlManagerV1, ()>,
{
    fn bind(
        _state: &mut D,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<WpTearingControlManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        data_init.init(resource, ());
    }
}

impl<D> Dispatch<WpTearingControlManagerV1, (), D> for TearingControlState
where
    D: Dispatch<WpTearingControlManagerV1, ()>
        + Dispatch<WpTearingControlV1, Arc<TearingControlData>>
        + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        manager: &WpTearingControlManagerV1,
        request: wp_tearing_control_manager_v1::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_tearing_control_manager_v1::Request::GetTearingControl { id, surface } => {
                // One per surface. A second is a client bug rather than a race:
                // it already holds the first, and answering both would leave two
                // objects disagreeing about one piece of surface state.
                let taken = with_states(&surface, |states| {
                    states
                        .data_map
                        .insert_if_missing_threadsafe(|| AtomicBool::new(false));
                    states
                        .data_map
                        .get::<AtomicBool>()
                        .is_some_and(|taken| taken.swap(true, Ordering::SeqCst))
                });
                if taken {
                    manager.post_error(
                        wp_tearing_control_manager_v1::Error::TearingControlExists,
                        "this surface already has a wp_tearing_control_v1",
                    );
                    return;
                }
                data_init.init(id, Arc::new(TearingControlData { surface }));
            }
            wp_tearing_control_manager_v1::Request::Destroy => {}
            _ => {}
        }
    }
}

impl<D> Dispatch<WpTearingControlV1, Arc<TearingControlData>, D> for TearingControlState
where
    D: Dispatch<WpTearingControlV1, Arc<TearingControlData>>,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &WpTearingControlV1,
        request: wp_tearing_control_v1::Request,
        data: &Arc<TearingControlData>,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            wp_tearing_control_v1::Request::SetPresentationHint { hint } => {
                let tearing = matches!(hint.into_result(), Ok(PresentationHint::Async));
                set_pending(&data.surface, tearing);
            }
            // "The compositor should stop tearing": the surface goes back to
            // waiting for the retrace, and it does so through the same
            // double-buffered path, so it takes effect with a commit like every
            // other change rather than mid-frame.
            wp_tearing_control_v1::Request::Destroy => {
                set_pending(&data.surface, false);
                with_states(&data.surface, |states| {
                    if let Some(taken) = states.data_map.get::<AtomicBool>() {
                        taken.store(false, Ordering::SeqCst);
                    }
                });
            }
            _ => {}
        }
    }
}

fn set_pending(surface: &WlSurface, tearing: bool) {
    if !surface.alive() {
        return;
    }
    with_states(surface, |states| {
        states
            .cached_state
            .get::<TearingCachedState>()
            .pending()
            .tearing = tearing;
    });
}

#[macro_export]
macro_rules! delegate_tearing_control {
    ($ty:ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($ty: [
            smithay::reexports::wayland_protocols::wp::tearing_control::v1::server::wp_tearing_control_manager_v1::WpTearingControlManagerV1: ()
        ] => $crate::tearing::TearingControlState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols::wp::tearing_control::v1::server::wp_tearing_control_manager_v1::WpTearingControlManagerV1: ()
        ] => $crate::tearing::TearingControlState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols::wp::tearing_control::v1::server::wp_tearing_control_v1::WpTearingControlV1: std::sync::Arc<$crate::tearing::TearingControlData>
        ] => $crate::tearing::TearingControlState);
    };
}
