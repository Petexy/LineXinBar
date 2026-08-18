//! Bindings for `lxb_shell_v1`, the private protocol LineXinBar's
//! compositor and its shell use to talk to each other.
//!
//! Both sides are generated from the same XML by `wayland-scanner`, so the two
//! halves of the session cannot drift apart. See
//! `protocols/lxb-shell-v1.xml` for what the interface actually says.
//!
//! The glob imports below are what the generated code resolves `wl_output`
//! through; the scanner emits unqualified paths and expects the core protocol
//! to be in scope.

pub mod overview;
pub mod wallpaper;

#[cfg(feature = "client")]
pub mod client {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/lxb-shell-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/lxb-shell-v1.xml");
}

#[cfg(feature = "server")]
pub mod server {
    use wayland_server;
    use wayland_server::protocol::*;

    pub mod __interfaces {
        use wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/lxb-shell-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_server_code!("protocols/lxb-shell-v1.xml");

    /// `frog_color_management_v1`, vendored rather than private.
    ///
    /// Not LineXinBar's protocol and not in wayland-protocols either: it is
    /// the interface Valve's HDR Vulkan layer and Gamescope actually speak,
    /// which is what makes it the one an HDR game under Proton will find. It
    /// is carried here for the same reason every other asset is — a session
    /// may have no desktop to borrow one from.
    pub mod frog {
        use wayland_server;
        use wayland_server::protocol::*;

        pub mod __interfaces {
            use wayland_server::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("protocols/frog-color-management-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_server_code!("protocols/frog-color-management-v1.xml");
    }
}
