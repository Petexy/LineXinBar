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
//! Applications may publish text fields, but only the session shell may read
//! surrounding text or inject keys. The portal has no input privileges.

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::input_method::{InputMethodHandler, InputMethodManagerState, PopupSurface};
use smithay::wayland::seat::WaylandFocus;
use smithay::wayland::text_input::TextInputManagerState;
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;

use crate::state::LxbState;

/// Advertise text input, input methods and virtual keyboards.
///
/// All three or none: a text input with nothing listening tells no one that a
/// field was focused, and an input method that cannot type is a notification.
pub fn advertise(display: &DisplayHandle) {
    TextInputManagerState::new::<LxbState>(display);
    InputMethodManagerState::new::<LxbState, _>(display, may_input);
    VirtualKeyboardManagerState::new::<LxbState, _>(display, may_input);
}

fn may_input(client: &smithay::reexports::wayland_server::Client) -> bool {
    crate::shell_control::role_of(client) == Some(crate::shell_control::Role::Shell)
}

/// The candidate window an input method may ask to have placed for it.
///
/// LineXinBar's shell never asks. It draws its keyboard on a layer surface of
/// its own, at the foot of the display, where a console keyboard belongs —
/// and a layer surface is something the compositor already knows how to
/// stack, unlike a popup that would have to be tracked against a text cursor
/// the shell cannot see. The requests are answered rather than ignored so
/// that an input method which does want one fails visibly, in the log, rather
/// than by drawing nothing for reasons nobody can find.
impl InputMethodHandler for LxbState {
    fn new_popup(&mut self, _surface: PopupSurface) {
        tracing::debug!("an input method asked for a popup; LineXinBar does not place them");
    }

    fn popup_repositioned(&mut self, _surface: PopupSurface) {}

    fn dismiss_popup(&mut self, _surface: PopupSurface) {}

    /// Where the window being typed into is, which is what a popup would be
    /// positioned against.
    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        self.lxb
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

smithay::delegate_text_input_manager!(LxbState);
smithay::delegate_input_method_manager!(LxbState);
smithay::delegate_virtual_keyboard_manager!(LxbState);

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::reexports::wayland_server::{backend::ClientData, Display};
    use std::sync::Arc;

    #[test]
    fn input_globals_reject_an_ordinary_client() {
        let display = Display::<LxbState>::new().unwrap();
        let mut handle = display.handle();
        let (server, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let data: Arc<dyn ClientData> = Arc::new(crate::state::ClientState::default());
        let client = handle.insert_client(server, data.clone()).unwrap();
        assert!(!may_input(&client));
    }

    /// The other half of the same rule, which is the half a session notices:
    /// the shell still gets both globals, and the portal still gets the frames
    /// it exists to hand over.
    ///
    /// Written as one test rather than four because [`PRIVILEGED`] is one
    /// static for the whole process: two tests each naming a shell would race
    /// each other over it. Everything else that reads it does so through a
    /// client with no peer credentials at all, which is refused whatever the
    /// static says.
    ///
    /// [`PRIVILEGED`]: crate::shell_control
    #[test]
    fn the_shell_may_type_and_the_portal_may_only_look() {
        use crate::shell_control::{
            the_session_portal_has_gone, the_session_shell_has_gone, this_is_the_session_portal,
            this_is_the_session_shell,
        };
        use smithay::reexports::wayland_server::GlobalDispatch;
        use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1;

        let display = Display::<LxbState>::new().unwrap();
        let mut handle = display.handle();
        let connect = |handle: &mut smithay::reexports::wayland_server::DisplayHandle, pid| {
            let (server, peer) = std::os::unix::net::UnixStream::pair().unwrap();
            let data: Arc<dyn ClientData> = Arc::new(crate::state::ClientState {
                pid: Some(pid),
                ..Default::default()
            });
            // The far end is held for as long as the client is, or the
            // connection is closed under it before anything is asked.
            (handle.insert_client(server, data).unwrap(), peer)
        };

        this_is_the_session_shell(4242);
        this_is_the_session_portal(9001);
        let (shell, _shell_peer) = connect(&mut handle, 4242);
        let (portal, _portal_peer) = connect(&mut handle, 9001);
        let (steam, _steam_peer) = connect(&mut handle, 31337);

        let may_capture = |client: &smithay::reexports::wayland_server::Client| {
            <LxbState as GlobalDispatch<ZwlrScreencopyManagerV1, ()>>::can_view(client.clone(), &())
        };

        // The screen: the shell photographs it, the portal hands frames to an
        // application that was granted them, and nothing else sees the global.
        assert!(may_capture(&shell));
        assert!(may_capture(&portal));
        assert!(!may_capture(&steam));

        // The keys: the shell alone. The portal talks to untrusted
        // applications for a living and has no business typing.
        assert!(may_input(&shell));
        assert!(!may_input(&portal));
        assert!(!may_input(&steam));

        the_session_shell_has_gone();
        the_session_portal_has_gone();
        assert!(!may_capture(&shell));
        assert!(!may_input(&shell));
        assert!(!may_capture(&portal));
    }
}
