//! Who is on the other end of a backend call.
//!
//! Everything in this crate is an `org.freedesktop.impl.portal.*` interface,
//! and the word in the middle is the whole of the problem. An *impl* portal is
//! not meant to be spoken to by applications. It is the private half of a pair:
//! `xdg-desktop-portal` — the front desk every desktop shares — takes the
//! question from the application, works out who is asking and what they are
//! allowed, and only then hands it down here. The arguments that arrive are
//! therefore the front desk's account of the application, not the
//! application's account of itself: `app_id` is the name the front desk
//! established, and `session_handle` names a conversation it is keeping track
//! of.
//!
//! None of that survives if something else on the bus calls these interfaces
//! directly. A session bus is reachable by every process of this user, which
//! in a LineXinBar session means Valve's client, every game it starts and
//! whatever the software hub installed. Any of them could call `CreateSession`
//! with an `app_id` of its choosing and have the user shown a consent panel
//! naming an application that is not asking, or call `Close` on a session
//! handle it guessed and end somebody else's share. The interfaces are
//! published on the bus, so there is nothing to stop the call arriving; what
//! there is, is the sender the bus stamps on every message, which no caller
//! can forge.
//!
//! ## What this proves and what it does not
//!
//! That the message came from the process which currently answers for
//! `org.freedesktop.portal.Desktop`. That is a strong statement about *which
//! connection* — the bus assigns unique names and never reissues them — and a
//! weak one about what that process is. A front desk is whatever claimed the
//! name first; this check is the boundary between the portal pair and the rest
//! of the bus, and not a judgement about the front desk itself.
//!
//! Properties are deliberately left open. `AvailableSourceTypes` and
//! `AvailableCursorModes` say what this backend can do, which is public and
//! constant, and the front desk reads them at *its* startup — possibly before
//! it owns the name it would be recognised by. A check on those would refuse
//! the one call that has to work, and would refuse it silently: see the
//! cached-capabilities note in `docs/desktop-integration.md`.

/// Whether this message came from the process answering for the portal.
///
/// Asked of the bus rather than of the message: `GetNameOwner` says which
/// connection holds `org.freedesktop.portal.Desktop` at this moment, and the
/// sender on the header is put there by the bus and cannot be written by the
/// caller. A message with no sender is not on a bus at all and is refused, as
/// is anything that goes wrong on the way to the answer — every caller of this
/// refuses on `false`, so an uncertain answer must be the refusing one.
pub async fn is_frontend(
    connection: &zbus::Connection,
    header: &zbus::message::Header<'_>,
) -> bool {
    let Some(sender) = header.sender() else {
        return false;
    };
    let Ok(bus) = zbus::fdo::DBusProxy::new(connection).await else {
        return false;
    };
    let name = zbus::names::BusName::from_static_str("org.freedesktop.portal.Desktop")
        .expect("a fixed bus name");
    match bus.get_name_owner(name).await {
        Ok(owner) => owner.as_str() == sender.as_str(),
        Err(_) => false,
    }
}

/// Whether this message may end a session that `opened_by` opened.
///
/// The connection that opened it, or whoever answers for the portal now. The
/// first is the stricter test of the two — a unique name belongs to one
/// connection for the life of the bus and is never given out again — and it is
/// tried first so that a front desk which has already lost the well-known name
/// can still close what it opened. See `screencast::Session::close` for why
/// that matters more here than anywhere else in this crate.
pub async fn may_close(
    connection: &zbus::Connection,
    header: &zbus::message::Header<'_>,
    opened_by: Option<&zbus::names::OwnedUniqueName>,
) -> bool {
    let opened_this = match (opened_by, header.sender()) {
        (Some(opened_by), Some(sender)) => opened_by.as_str() == sender.as_str(),
        _ => false,
    };
    opened_this || is_frontend(connection, header).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::process::{Command, Stdio};

    struct PrivateBus(std::process::Child);
    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    // A real isolated bus supplies the sender name; no user session names are
    // acquired by this test, and no screen or file prompt is opened.
    //
    // The daemon is pointed at a config of this test's own rather than started
    // with `--session`: that flag has it read the system's
    // `/etc/dbus-1/session.conf`, which a build sandbox does not have — and
    // which a distro may have configured in ways a test of who may own
    // `org.freedesktop.portal.Desktop` should not depend on. The address is
    // printed only once the config is loaded, so the file can go again as soon
    // as it has been read.
    #[test]
    fn only_the_front_desk_is_trusted_and_only_its_own_may_close() {
        let config = std::env::temp_dir().join(format!(
            "lxb-portal-caller-test-bus-{}.conf",
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
        zbus::block_on(async {
            let backend = zbus::connection::Builder::address(address.trim())
                .unwrap()
                .build()
                .await
                .unwrap();
            let frontend = zbus::connection::Builder::address(address.trim())
                .unwrap()
                .build()
                .await
                .unwrap();
            let impostor = zbus::connection::Builder::address(address.trim())
                .unwrap()
                .build()
                .await
                .unwrap();
            let message = |connection: &zbus::Connection| {
                zbus::Message::method_call("/org/linexinbar/Test", "Test")
                    .unwrap()
                    .sender(connection.unique_name().unwrap())
                    .unwrap()
                    .build(&())
                    .unwrap()
            };
            let real = message(&frontend);
            let fake = message(&impostor);
            assert!(!is_frontend(&backend, &real.header()).await);
            frontend
                .request_name("org.freedesktop.portal.Desktop")
                .await
                .unwrap();
            assert!(is_frontend(&backend, &real.header()).await);
            assert!(!is_frontend(&backend, &fake.header()).await);
            frontend
                .release_name("org.freedesktop.portal.Desktop")
                .await
                .unwrap();
            impostor
                .request_name("org.freedesktop.portal.Desktop")
                .await
                .unwrap();
            assert!(!is_frontend(&backend, &real.header()).await);
            assert!(is_frontend(&backend, &fake.header()).await);

            // Closing a session is the one question whose refusal is not the
            // safe answer — a `Close` that does not land leaves the screen
            // being read. The front desk that opened this one has just lost
            // the well-known name, which is exactly what a portal restart
            // does, and it must still be able to end what it started.
            let opened_by: zbus::names::OwnedUniqueName =
                frontend.unique_name().unwrap().to_owned();
            assert!(may_close(&backend, &real.header(), Some(&opened_by)).await);

            // Whoever answers for the portal now may tidy up after it, and
            // nobody else may: a third connection cannot end somebody's share.
            assert!(may_close(&backend, &fake.header(), Some(&opened_by)).await);
            let stranger = zbus::connection::Builder::address(address.trim())
                .unwrap()
                .build()
                .await
                .unwrap();
            let theirs = message(&stranger);
            assert!(!may_close(&backend, &theirs.header(), Some(&opened_by)).await);

            // And a session whose opener was never recorded falls back to the
            // name, rather than to letting anyone through.
            assert!(!may_close(&backend, &theirs.header(), None).await);
            assert!(may_close(&backend, &fake.header(), None).await);
        });
    }
}
