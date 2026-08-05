//! Bindings for `linboard_shell_v1`, the private protocol Linboard's
//! compositor and its shell use to talk to each other.
//!
//! Both sides are generated from the same XML by `wayland-scanner`, so the two
//! halves of the session cannot drift apart. See
//! `protocols/linboard-shell-v1.xml` for what the interface actually says.
//!
//! The glob imports below are what the generated code resolves `wl_output`
//! through; the scanner emits unqualified paths and expects the core protocol
//! to be in scope.

pub mod overview;

#[cfg(feature = "client")]
pub mod client {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/linboard-shell-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/linboard-shell-v1.xml");
}

#[cfg(feature = "server")]
pub mod server {
    use wayland_server;
    use wayland_server::protocol::*;

    pub mod __interfaces {
        use wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/linboard-shell-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_server_code!("protocols/linboard-shell-v1.xml");
}
