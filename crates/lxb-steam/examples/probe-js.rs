//! One expression, evaluated in Valve's client, with the answer printed.
//!
//! The client's whole interface is JavaScript over an IPC to `steamclient.so`
//! (see [`lxb_steam::webui`]), so nearly every question about what the client
//! thinks is a question this can ask. It exists because the alternative, when
//! something in that half is wrong, is adding a call to the crate and building
//! the shell to find out whether it was the right call.
//!
//! It is also how the guide-button setting was found: reading Valve's own
//! settings table for the field number, then watching
//! `GetDesiredSteamUIWindows` go from the desktop window to Big Picture's while
//! somebody pressed the button.
//!
//! Needs a client that is exposing its context — `probe-client --context` puts
//! the marker up and starts one.
//!
//! ```text
//! cargo run -p lxb-steam --example probe-js -- 'SteamClient.UI.GetUIMode()'
//! cargo run -p lxb-steam --example probe-js -- \
//!     'String(window.settingsStore.m_ClientSettings.controller_guide_button_focus_steam)'
//! ```

use std::time::Duration;

/// Long enough for a call that waits on the C++ half, and short enough that a
/// client which has stopped answering is a failure rather than a hang.
const PATIENCE: Duration = Duration::from_secs(30);

fn main() {
    let expression = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    if expression.trim().is_empty() {
        eprintln!("usage: probe-js '<javascript>'");
        std::process::exit(2);
    }

    let Some(socket) = shared_context() else {
        eprintln!(
            "no client is exposing its JS context on 127.0.0.1:8080 — \
             start one with `cargo run -p lxb-steam --example probe-client -- --context`"
        );
        std::process::exit(1);
    };

    let (mut stream, _) = tungstenite::connect(&socket).expect("the debugger would not connect");
    if let tungstenite::stream::MaybeTlsStream::Plain(plain) = stream.get_ref() {
        let _ = plain.set_read_timeout(Some(PATIENCE));
    }
    let request = serde_json::json!({
        "id": 1,
        "method": "Runtime.evaluate",
        // `awaitPromise`, because every `SteamClient` method answers with one.
        "params": { "expression": expression, "awaitPromise": true, "returnByValue": true },
    });
    stream
        .send(tungstenite::Message::Text(request.to_string().into()))
        .expect("the request would not send");

    // The protocol interleaves events with answers, so this reads until the
    // answer to *this* request arrives rather than taking the first message.
    loop {
        let Ok(tungstenite::Message::Text(text)) = stream.read() else {
            continue;
        };
        let Ok(answer) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if answer["id"] != 1 {
            continue;
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&answer["result"]).unwrap_or(text.to_string())
        );
        return;
    }
}

/// The debugger address of the page every `SteamClient` call goes through.
fn shared_context() -> Option<String> {
    let listing = ureq::get("http://127.0.0.1:8080/json")
        .config()
        .timeout_global(Some(PATIENCE))
        .build()
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;
    serde_json::from_str::<Vec<serde_json::Value>>(&listing)
        .ok()?
        .iter()
        .find(|page| page["title"] == "SharedJSContext")
        .and_then(|page| page["webSocketDebuggerUrl"].as_str())
        .map(str::to_string)
}
