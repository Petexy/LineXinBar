//! Typing without a keyboard: the three protocols an on-screen keyboard needs.
//!
//! `zwp_text_input_v3` is how an application says one of its text fields has
//! the cursor. `zwp_input_method_v2` is how the shell hears about it — and
//! that event is the only signal Wayland has that a keyboard should come up
//! at all, because nothing else in the protocol describes what is *inside* a
//! window. `zwp_virtual_keyboard_v1` is how the shell then types: it uploads
//! a keymap of its own and sends keycodes through the seat, which is what
//! makes the keys arrive at a terminal, a game and an X11 client as well as
//! at the field that asked for them.
//!
//! Smithay wires the first two to each other and follows keyboard focus by
//! itself, so there is nothing to do here but advertise the globals and
//! answer for the popups.
//!
//! Which clients may bind them is the same question `linboard_shell_v1`
//! already answers, and gets the same answer: this compositor only ever runs
//! clients the user's own session started, so it does not pretend to sort
//! them into privileged and not.

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::input_method::{InputMethodHandler, InputMethodManagerState, PopupSurface};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::text_input::TextInputManagerState;
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;

use crate::state::LinboardState;

/// Advertise text input, input methods and virtual keyboards.
///
/// All three or none: a text input with nothing listening tells no one that a
/// field was focused, and an input method that cannot type is a notification.
pub fn advertise(display: &DisplayHandle) {
    TextInputManagerState::new::<LinboardState>(display);
    InputMethodManagerState::new::<LinboardState, _>(display, |_client| true);
    VirtualKeyboardManagerState::new::<LinboardState, _>(display, |_client| true);
}

/// The candidate window an input method may ask to have placed for it.
///
/// Linboard's shell never asks. It draws its keyboard on a layer surface of
/// its own, at the foot of the display, where a console keyboard belongs —
/// and a layer surface is something the compositor already knows how to
/// stack, unlike a popup that would have to be tracked against a text cursor
/// the shell cannot see. The requests are answered rather than ignored so
/// that an input method which does want one fails visibly, in the log, rather
/// than by drawing nothing for reasons nobody can find.
impl InputMethodHandler for LinboardState {
    fn new_popup(&mut self, _surface: PopupSurface) {
        tracing::debug!("an input method asked for a popup; Linboard does not place them");
    }

    fn popup_repositioned(&mut self, _surface: PopupSurface) {}

    fn dismiss_popup(&mut self, _surface: PopupSurface) {}

    /// Where the window being typed into is, which is what a popup would be
    /// positioned against.
    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        self.linboard
            .space
            .elements()
            .find(|window| {
                window
                    .wl_surface()
                    .is_some_and(|surface| surface.as_ref() == parent)
            })
            .map(|window| window.geometry())
            .unwrap_or_default()
    }
}

smithay::delegate_text_input_manager!(LinboardState);
smithay::delegate_input_method_manager!(LinboardState);
smithay::delegate_virtual_keyboard_manager!(LinboardState);
