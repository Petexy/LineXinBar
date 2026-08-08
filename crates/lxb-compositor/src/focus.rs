//! Keyboard focus targets shared by native Wayland and XWayland windows.
//!
//! Pointer and touch events can continue to target the associated
//! `wl_surface`, but keyboard focus must retain the X11 window wrapper so the
//! XWM can update the X input focus and honour `WM_TAKE_FOCUS`.

use std::borrow::Cow;

use smithay::backend::input::KeyState;
use smithay::desktop::PopupKind;
use smithay::input::keyboard::{KeyboardTarget, KeysymHandle, ModifiersState};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent,
    GesturePinchEndEvent, GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent,
    GestureSwipeUpdateEvent, MotionEvent, PointerTarget, RelativeMotionEvent,
};
use smithay::input::touch::{
    DownEvent, MotionEvent as TouchMotionEvent, OrientationEvent, ShapeEvent, TouchTarget, UpEvent,
};
use smithay::input::{Seat, SeatHandler};
use smithay::reexports::wayland_server::backend::ObjectId;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Serial};
use smithay::wayland::seat::WaylandFocus;
use smithay::xwayland::X11Surface;

/// Anything the seat may give keyboard focus to.
#[derive(Debug, Clone)]
pub enum KeyboardFocusTarget {
    Wayland(WlSurface),
    X11(X11Surface),
}

/// Compare X11 surfaces by their protocol identity rather than Smithay's
/// `PartialEq`. Smithay intentionally makes a destroyed `X11Surface` unequal
/// to every surface (including its own clones), but focus/window cleanup must
/// still be able to find that dead window after `destroyed_window` arrives.
pub(crate) fn x11_surface_matches(left: &X11Surface, right: &X11Surface) -> bool {
    left.xwm_id() == right.xwm_id() && left.window_id() == right.window_id()
}

impl PartialEq for KeyboardFocusTarget {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Wayland(left), Self::Wayland(right)) => left == right,
            (Self::X11(left), Self::X11(right)) => x11_surface_matches(left, right),
            _ => false,
        }
    }
}

impl From<WlSurface> for KeyboardFocusTarget {
    fn from(surface: WlSurface) -> Self {
        Self::Wayland(surface)
    }
}

impl From<X11Surface> for KeyboardFocusTarget {
    fn from(surface: X11Surface) -> Self {
        Self::X11(surface)
    }
}

impl From<PopupKind> for KeyboardFocusTarget {
    fn from(popup: PopupKind) -> Self {
        Self::Wayland(popup.into())
    }
}

impl IsAlive for KeyboardFocusTarget {
    fn alive(&self) -> bool {
        match self {
            Self::Wayland(surface) => surface.alive(),
            Self::X11(surface) => surface.alive(),
        }
    }
}

impl WaylandFocus for KeyboardFocusTarget {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        match self {
            Self::Wayland(surface) => Some(Cow::Borrowed(surface)),
            Self::X11(surface) => surface.wl_surface().map(Cow::Owned),
        }
    }

    fn same_client_as(&self, object_id: &ObjectId) -> bool {
        match self {
            Self::Wayland(surface) => surface.same_client_as(object_id),
            Self::X11(surface) => surface.same_client_as(object_id),
        }
    }
}

impl<D> KeyboardTarget<D> for KeyboardFocusTarget
where
    D: SeatHandler<KeyboardFocus = Self> + 'static,
{
    fn enter(&self, seat: &Seat<D>, data: &mut D, keys: Vec<KeysymHandle<'_>>, serial: Serial) {
        match self {
            Self::Wayland(surface) => KeyboardTarget::<D>::enter(surface, seat, data, keys, serial),
            Self::X11(surface) => KeyboardTarget::<D>::enter(surface, seat, data, keys, serial),
        }
    }

    fn leave(&self, seat: &Seat<D>, data: &mut D, serial: Serial) {
        match self {
            Self::Wayland(surface) => KeyboardTarget::<D>::leave(surface, seat, data, serial),
            Self::X11(surface) => KeyboardTarget::<D>::leave(surface, seat, data, serial),
        }
    }

    fn key(
        &self,
        seat: &Seat<D>,
        data: &mut D,
        key: KeysymHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: u32,
    ) {
        match self {
            Self::Wayland(surface) => {
                KeyboardTarget::<D>::key(surface, seat, data, key, state, serial, time)
            }
            Self::X11(surface) => {
                KeyboardTarget::<D>::key(surface, seat, data, key, state, serial, time)
            }
        }
    }

    fn modifiers(&self, seat: &Seat<D>, data: &mut D, modifiers: ModifiersState, serial: Serial) {
        match self {
            Self::Wayland(surface) => {
                KeyboardTarget::<D>::modifiers(surface, seat, data, modifiers, serial)
            }
            Self::X11(surface) => {
                KeyboardTarget::<D>::modifiers(surface, seat, data, modifiers, serial)
            }
        }
    }
}

impl<D> PointerTarget<D> for KeyboardFocusTarget
where
    D: SeatHandler<PointerFocus = Self> + 'static,
{
    fn enter(&self, seat: &Seat<D>, data: &mut D, event: &MotionEvent) {
        match self {
            Self::Wayland(target) => PointerTarget::<D>::enter(target, seat, data, event),
            Self::X11(target) => PointerTarget::<D>::enter(target, seat, data, event),
        }
    }

    fn motion(&self, seat: &Seat<D>, data: &mut D, event: &MotionEvent) {
        match self {
            Self::Wayland(target) => PointerTarget::<D>::motion(target, seat, data, event),
            Self::X11(target) => PointerTarget::<D>::motion(target, seat, data, event),
        }
    }

    fn relative_motion(&self, seat: &Seat<D>, data: &mut D, event: &RelativeMotionEvent) {
        match self {
            Self::Wayland(target) => PointerTarget::<D>::relative_motion(target, seat, data, event),
            Self::X11(target) => PointerTarget::<D>::relative_motion(target, seat, data, event),
        }
    }

    fn button(&self, seat: &Seat<D>, data: &mut D, event: &ButtonEvent) {
        match self {
            Self::Wayland(target) => PointerTarget::<D>::button(target, seat, data, event),
            Self::X11(target) => PointerTarget::<D>::button(target, seat, data, event),
        }
    }

    fn axis(&self, seat: &Seat<D>, data: &mut D, frame: AxisFrame) {
        match self {
            Self::Wayland(target) => PointerTarget::<D>::axis(target, seat, data, frame),
            Self::X11(target) => PointerTarget::<D>::axis(target, seat, data, frame),
        }
    }

    fn frame(&self, seat: &Seat<D>, data: &mut D) {
        match self {
            Self::Wayland(target) => PointerTarget::<D>::frame(target, seat, data),
            Self::X11(target) => PointerTarget::<D>::frame(target, seat, data),
        }
    }

    fn gesture_swipe_begin(&self, seat: &Seat<D>, data: &mut D, event: &GestureSwipeBeginEvent) {
        match self {
            Self::Wayland(target) => {
                PointerTarget::<D>::gesture_swipe_begin(target, seat, data, event)
            }
            Self::X11(target) => PointerTarget::<D>::gesture_swipe_begin(target, seat, data, event),
        }
    }

    fn gesture_swipe_update(&self, seat: &Seat<D>, data: &mut D, event: &GestureSwipeUpdateEvent) {
        match self {
            Self::Wayland(target) => {
                PointerTarget::<D>::gesture_swipe_update(target, seat, data, event)
            }
            Self::X11(target) => {
                PointerTarget::<D>::gesture_swipe_update(target, seat, data, event)
            }
        }
    }

    fn gesture_swipe_end(&self, seat: &Seat<D>, data: &mut D, event: &GestureSwipeEndEvent) {
        match self {
            Self::Wayland(target) => {
                PointerTarget::<D>::gesture_swipe_end(target, seat, data, event)
            }
            Self::X11(target) => PointerTarget::<D>::gesture_swipe_end(target, seat, data, event),
        }
    }

    fn gesture_pinch_begin(&self, seat: &Seat<D>, data: &mut D, event: &GesturePinchBeginEvent) {
        match self {
            Self::Wayland(target) => {
                PointerTarget::<D>::gesture_pinch_begin(target, seat, data, event)
            }
            Self::X11(target) => PointerTarget::<D>::gesture_pinch_begin(target, seat, data, event),
        }
    }

    fn gesture_pinch_update(&self, seat: &Seat<D>, data: &mut D, event: &GesturePinchUpdateEvent) {
        match self {
            Self::Wayland(target) => {
                PointerTarget::<D>::gesture_pinch_update(target, seat, data, event)
            }
            Self::X11(target) => {
                PointerTarget::<D>::gesture_pinch_update(target, seat, data, event)
            }
        }
    }

    fn gesture_pinch_end(&self, seat: &Seat<D>, data: &mut D, event: &GesturePinchEndEvent) {
        match self {
            Self::Wayland(target) => {
                PointerTarget::<D>::gesture_pinch_end(target, seat, data, event)
            }
            Self::X11(target) => PointerTarget::<D>::gesture_pinch_end(target, seat, data, event),
        }
    }

    fn gesture_hold_begin(&self, seat: &Seat<D>, data: &mut D, event: &GestureHoldBeginEvent) {
        match self {
            Self::Wayland(target) => {
                PointerTarget::<D>::gesture_hold_begin(target, seat, data, event)
            }
            Self::X11(target) => PointerTarget::<D>::gesture_hold_begin(target, seat, data, event),
        }
    }

    fn gesture_hold_end(&self, seat: &Seat<D>, data: &mut D, event: &GestureHoldEndEvent) {
        match self {
            Self::Wayland(target) => {
                PointerTarget::<D>::gesture_hold_end(target, seat, data, event)
            }
            Self::X11(target) => PointerTarget::<D>::gesture_hold_end(target, seat, data, event),
        }
    }

    fn leave(&self, seat: &Seat<D>, data: &mut D, serial: Serial, time: u32) {
        match self {
            Self::Wayland(target) => PointerTarget::<D>::leave(target, seat, data, serial, time),
            Self::X11(target) => PointerTarget::<D>::leave(target, seat, data, serial, time),
        }
    }
}

impl<D> TouchTarget<D> for KeyboardFocusTarget
where
    D: SeatHandler<TouchFocus = Self> + 'static,
{
    fn down(&self, seat: &Seat<D>, data: &mut D, event: &DownEvent, seq: Serial) {
        match self {
            Self::Wayland(target) => TouchTarget::<D>::down(target, seat, data, event, seq),
            Self::X11(target) => TouchTarget::<D>::down(target, seat, data, event, seq),
        }
    }

    fn up(&self, seat: &Seat<D>, data: &mut D, event: &UpEvent, seq: Serial) {
        match self {
            Self::Wayland(target) => TouchTarget::<D>::up(target, seat, data, event, seq),
            Self::X11(target) => TouchTarget::<D>::up(target, seat, data, event, seq),
        }
    }

    fn motion(&self, seat: &Seat<D>, data: &mut D, event: &TouchMotionEvent, seq: Serial) {
        match self {
            Self::Wayland(target) => TouchTarget::<D>::motion(target, seat, data, event, seq),
            Self::X11(target) => TouchTarget::<D>::motion(target, seat, data, event, seq),
        }
    }

    fn frame(&self, seat: &Seat<D>, data: &mut D, seq: Serial) {
        match self {
            Self::Wayland(target) => TouchTarget::<D>::frame(target, seat, data, seq),
            Self::X11(target) => TouchTarget::<D>::frame(target, seat, data, seq),
        }
    }

    fn cancel(&self, seat: &Seat<D>, data: &mut D, seq: Serial) {
        match self {
            Self::Wayland(target) => TouchTarget::<D>::cancel(target, seat, data, seq),
            Self::X11(target) => TouchTarget::<D>::cancel(target, seat, data, seq),
        }
    }

    fn shape(&self, seat: &Seat<D>, data: &mut D, event: &ShapeEvent, seq: Serial) {
        match self {
            Self::Wayland(target) => TouchTarget::<D>::shape(target, seat, data, event, seq),
            Self::X11(target) => TouchTarget::<D>::shape(target, seat, data, event, seq),
        }
    }

    fn orientation(&self, seat: &Seat<D>, data: &mut D, event: &OrientationEvent, seq: Serial) {
        match self {
            Self::Wayland(target) => TouchTarget::<D>::orientation(target, seat, data, event, seq),
            Self::X11(target) => TouchTarget::<D>::orientation(target, seat, data, event, seq),
        }
    }
}
