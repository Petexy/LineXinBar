//! Keeping the screen on and the machine awake, for a program in a sandbox.
//!
//! A program outside a sandbox asks the session directly: the shell answers
//! `org.freedesktop.ScreenSaver` for the screen and
//! `org.freedesktop.PowerManagement.Inhibit` for sleep, and a film player or a
//! game that asks is what holds the screen lit and the machine awake — see the
//! shell's `power_bus` and `idle`. A program in a Flatpak cannot see those
//! names. It asks the desktop portal instead, and the portal hands the question
//! to whichever backend the desktop installed for Inhibit — which, left to the
//! machine's defaults, is another desktop's, talking to a session manager this
//! session has not got. So an emulator from Flathub, and every game Heroic's
//! Flatpak starts, asked to keep the machine awake and nobody heard.
//!
//! This backend carries the question the last step: an inhibition for *idle*
//! becomes the shell's screen inhibition, one for *suspend* becomes its sleep
//! inhibition, and closing the request the portal made for it lets both go.
//! Logging out and switching user are not this session's to hold, and are
//! left alone. The shell drops everything this process holds if it goes away,
//! so nothing outlives it.

use std::collections::HashMap;

use zbus::zvariant::{OwnedObjectPath, OwnedValue};

/// The portal's flags for what an inhibition holds.
const SUSPEND: u32 = 4;
const IDLE: u32 = 8;

/// The shell's two interfaces, as (bus name, object path, interface).
const SCREEN: (&str, &str, &str) = (
    "org.freedesktop.ScreenSaver",
    "/org/freedesktop/ScreenSaver",
    "org.freedesktop.ScreenSaver",
);
const SLEEP: (&str, &str, &str) = (
    "org.freedesktop.PowerManagement",
    "/org/freedesktop/PowerManagement/Inhibit",
    "org.freedesktop.PowerManagement.Inhibit",
);

/// The portal itself. What it holds lives on each request's own object.
pub struct Inhibit;

#[zbus::interface(name = "org.freedesktop.impl.portal.Inhibit")]
impl Inhibit {
    /// Hold what `flags` asks for until the request at `handle` is closed.
    // Two of these are the bus's own facts, injected by `zbus` rather than
    // sent by the caller. See [`crate::caller`].
    #[allow(clippy::too_many_arguments)]
    async fn inhibit(
        &self,
        handle: OwnedObjectPath,
        app_id: String,
        _window: String,
        flags: u32,
        options: HashMap<String, OwnedValue>,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if !crate::caller::is_frontend(connection, &header).await {
            return Err(zbus::fdo::Error::AccessDenied(
                "only the desktop portal may ask this".into(),
            ));
        }
        let reason = options
            .get("reason")
            .and_then(|value| String::try_from(value.clone()).ok())
            .unwrap_or_default();
        let application = if app_id.is_empty() {
            "an application".to_string()
        } else {
            app_id
        };
        let mut held = Vec::new();
        for (bit, place) in wanted(flags) {
            match hold(connection, place, &application, &reason).await {
                Some(cookie) => held.push((place, cookie)),
                None => tracing::info!(bit, application, "nothing on this bus holds that"),
            }
        }
        tracing::info!(
            application,
            reason,
            flags,
            held = held.len(),
            "a sandboxed program asked to stay awake"
        );
        let request = Request {
            held,
            opened_by: header.sender().map(|sender| sender.to_owned().into()),
            path: handle.clone(),
        };
        if let Err(err) = server.at(&handle, request).await {
            tracing::warn!(%err, "the request could not be kept, so nothing is held");
        }
        Ok(())
    }

    /// Watching the session end is not offered: this session has no session
    /// manager to say it is ending. 2 is the portal's word for "did not happen".
    async fn create_monitor(
        &self,
        _handle: OwnedObjectPath,
        _session_handle: OwnedObjectPath,
        _app_id: String,
        _window: String,
    ) -> u32 {
        2
    }

    async fn query_end_response(&self, _session_handle: OwnedObjectPath) {}
}

/// Which of the shell's two interfaces an inhibition's flags ask for.
fn wanted(flags: u32) -> Vec<(u32, (&'static str, &'static str, &'static str))> {
    [(IDLE, SCREEN), (SUSPEND, SLEEP)]
        .into_iter()
        .filter(|(bit, _)| flags & bit != 0)
        .collect()
}

async fn hold(
    connection: &zbus::Connection,
    (name, path, interface): (&str, &str, &str),
    application: &str,
    reason: &str,
) -> Option<u32> {
    connection
        .call_method(
            Some(name),
            path,
            Some(interface),
            "Inhibit",
            &(application, reason),
        )
        .await
        .ok()?
        .body()
        .deserialize::<u32>()
        .ok()
}

/// One inhibition, for as long as the portal keeps it open.
struct Request {
    held: Vec<((&'static str, &'static str, &'static str), u32)>,
    opened_by: Option<zbus::names::OwnedUniqueName>,
    path: OwnedObjectPath,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Request")]
impl Request {
    /// The program let go, or went away: let go of what it held.
    async fn close(
        &mut self,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        #[zbus(connection)] connection: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) {
        if !crate::caller::may_close(connection, &header, self.opened_by.as_ref()).await {
            tracing::warn!(request = %self.path, "something that did not ask tried to let go");
            return;
        }
        for ((name, path, interface), cookie) in self.held.drain(..) {
            let _ = connection
                .call_method(Some(name), path, Some(interface), "UnInhibit", &(cookie,))
                .await;
        }
        let _ = server.remove::<Request, _>(&self.path).await;
    }
}

/// Serve the portal on the backend's object, beside the others.
pub fn serve_at<'a>(
    builder: zbus::connection::Builder<'a>,
    path: &zbus::zvariant::ObjectPath<'a>,
) -> zbus::Result<zbus::connection::Builder<'a>> {
    builder.serve_at(path, Inhibit)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Idle is the screen, suspend is sleep, and logging out and switching
    /// user are nobody's here.
    #[test]
    fn idle_is_the_screen_and_suspend_is_sleep() {
        assert_eq!(wanted(IDLE), vec![(IDLE, SCREEN)]);
        assert_eq!(wanted(SUSPEND), vec![(SUSPEND, SLEEP)]);
        assert_eq!(wanted(IDLE | SUSPEND).len(), 2);
        assert!(wanted(1 | 2).is_empty());
    }

    struct PrivateBus(std::process::Child);
    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// What the stand-in for the shell was asked.
    #[derive(Default)]
    struct Heard {
        inhibited: Vec<(String, String)>,
        released: Vec<u32>,
    }

    struct StandIn(std::sync::Arc<std::sync::Mutex<Heard>>);

    #[zbus::interface(name = "org.freedesktop.ScreenSaver")]
    impl StandIn {
        fn inhibit(&self, application: String, reason: String) -> u32 {
            let mut heard = self.0.lock().unwrap();
            heard.inhibited.push((application, reason));
            heard.inhibited.len() as u32
        }

        #[zbus(name = "UnInhibit")]
        fn un_inhibit(&self, cookie: u32) {
            self.0.lock().unwrap().released.push(cookie);
        }
    }

    /// The whole road on a private bus: the front desk asks for idle, the
    /// shell's screen interface is asked to hold, and closing the request lets
    /// it go. Nothing on the user's own session bus is touched.
    #[test]
    fn a_request_holds_the_screen_until_it_is_closed() {
        use std::io::BufRead;
        use std::process::{Command, Stdio};
        let config = std::env::temp_dir().join(format!(
            "lxb-portal-inhibit-test-bus-{}.conf",
            std::process::id()
        ));
        std::fs::write(
            &config,
            concat!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\" ",
                "\"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n",
                "<busconfig>\n",
                "  <type>session</type>\n",
                "  <listen>unix:tmpdir=/tmp</listen>\n",
                "  <policy context=\"default\">\n",
                "    <allow send_destination=\"*\" eavesdrop=\"true\"/>\n",
                "    <allow eavesdrop=\"true\"/>\n",
                "    <allow own=\"*\"/>\n",
                "  </policy>\n",
                "</busconfig>\n",
            ),
        )
        .expect("write the bus config");
        let mut bus = PrivateBus(
            Command::new("dbus-daemon")
                .arg(format!("--config-file={}", config.display()))
                .args(["--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .expect("start a private D-Bus"),
        );
        let mut address = String::new();
        std::io::BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let _ = std::fs::remove_file(&config);
        let heard = std::sync::Arc::new(std::sync::Mutex::new(Heard::default()));
        zbus::block_on(async {
            let connect = || async {
                zbus::connection::Builder::address(address.trim())
                    .unwrap()
                    .build()
                    .await
                    .unwrap()
            };
            let _shell = zbus::connection::Builder::address(address.trim())
                .unwrap()
                .name("org.freedesktop.ScreenSaver")
                .unwrap()
                .serve_at("/org/freedesktop/ScreenSaver", StandIn(heard.clone()))
                .unwrap()
                .build()
                .await
                .unwrap();
            let path =
                zbus::zvariant::ObjectPath::try_from("/org/freedesktop/portal/desktop").unwrap();
            let backend = serve_at(
                zbus::connection::Builder::address(address.trim()).unwrap(),
                &path,
            )
            .unwrap()
            .name("org.freedesktop.impl.portal.desktop.lxb")
            .unwrap()
            .build()
            .await
            .unwrap();
            let frontend = connect().await;
            frontend
                .request_name("org.freedesktop.portal.Desktop")
                .await
                .unwrap();
            let handle = "/org/freedesktop/portal/desktop/request/1_1/game";
            let mut options: HashMap<&str, zbus::zvariant::Value> = HashMap::new();
            options.insert("reason", "Playing".into());
            frontend
                .call_method(
                    Some("org.freedesktop.impl.portal.desktop.lxb"),
                    "/org/freedesktop/portal/desktop",
                    Some("org.freedesktop.impl.portal.Inhibit"),
                    "Inhibit",
                    &(
                        zbus::zvariant::ObjectPath::try_from(handle).unwrap(),
                        "org.example.Game",
                        "",
                        IDLE,
                        options,
                    ),
                )
                .await
                .expect("the front desk is answered");
            assert_eq!(
                heard.lock().unwrap().inhibited,
                [("org.example.Game".to_string(), "Playing".to_string())]
            );

            // Somebody else on the bus cannot ask, and cannot let go for it.
            let stranger = connect().await;
            assert!(stranger
                .call_method(
                    Some("org.freedesktop.impl.portal.desktop.lxb"),
                    "/org/freedesktop/portal/desktop",
                    Some("org.freedesktop.impl.portal.Inhibit"),
                    "Inhibit",
                    &(
                        zbus::zvariant::ObjectPath::try_from(
                            "/org/freedesktop/portal/desktop/request/1_2/x"
                        )
                        .unwrap(),
                        "impostor",
                        "",
                        IDLE,
                        HashMap::<&str, zbus::zvariant::Value>::new(),
                    ),
                )
                .await
                .is_err());
            let _ = stranger
                .call_method(
                    Some("org.freedesktop.impl.portal.desktop.lxb"),
                    handle,
                    Some("org.freedesktop.impl.portal.Request"),
                    "Close",
                    &(),
                )
                .await;
            assert!(heard.lock().unwrap().released.is_empty());

            frontend
                .call_method(
                    Some("org.freedesktop.impl.portal.desktop.lxb"),
                    handle,
                    Some("org.freedesktop.impl.portal.Request"),
                    "Close",
                    &(),
                )
                .await
                .expect("the request is closed");
            assert_eq!(heard.lock().unwrap().released, [1]);
            drop(backend);
        });
    }
}
