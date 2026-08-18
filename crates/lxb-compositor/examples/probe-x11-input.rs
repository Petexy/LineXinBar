//! An X11 window that says what is being done to its input, standing in for a
//! game under Proton.
//!
//! A Wine game is the hardest client this compositor has to focus and the one
//! it is least able to ask afterwards. It declares ICCCM's *globally active*
//! input model — `WM_HINTS.input = False` beside `WM_TAKE_FOCUS` in
//! `WM_PROTOCOLS` — which tells the window manager not to set the focus itself
//! but to send a message and let the client do it. Everything that then goes
//! wrong goes wrong silently: the keyboard is handed over and nothing arrives,
//! the game sits there deaf, and from outside it is indistinguishable from a
//! game that has decided to ignore the user.
//!
//! So this says. It declares exactly what a Wine window declares, answers
//! `WM_TAKE_FOCUS` the way Wine answers it, and prints every focus change, key
//! and button it is given, with the X server's own timestamps. Run it inside a
//! nested session next to the shell and the log is the answer to "did the game
//! get its keyboard back": a `FocusIn` and a key, or neither.
//!
//! ```text
//! DISPLAY=:1 cargo run --release --example probe-x11-input -- [--no-take-focus]
//! ```
//!
//! `--no-take-focus` drops `WM_TAKE_FOCUS` from the protocols while keeping
//! `input = False`, which is ICCCM's *no input* model: a window that wants the
//! keyboard given to nobody. It is here because it is the one shape a
//! compositor must not silently treat as the globally active one.

use std::time::Instant;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt, CreateWindowAux, EventMask, InputFocus, PropMode, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::COPY_DEPTH_FROM_PARENT;

/// The window id following a flag, as `0x…` or plain hexadecimal.
fn argument(flag: &str) -> Option<u32> {
    std::env::args()
        .skip_while(|arg| arg != flag)
        .nth(1)
        .and_then(|arg| u32::from_str_radix(arg.trim_start_matches("0x"), 16).ok())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Where the X server itself thinks the pointer is, printed until it is
    // stopped. The one measurement that separates "the compositor never
    // delivered the motion" from "it did, and the client ignored it": XWayland
    // moves its own pointer only when it is told, so an X pointer that stands
    // still is a client that was never sent anything.
    if std::env::args().any(|arg| arg == "--watch-pointer") {
        let (conn, screen_num) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen_num].root;
        let mut last = None;
        let started = Instant::now();
        loop {
            let at = conn.query_pointer(root)?.reply()?;
            let now = (at.root_x, at.root_y);
            if Some(now) != last {
                println!(
                    "{:7.3}s pointer at {},{}",
                    started.elapsed().as_secs_f32(),
                    now.0,
                    now.1
                );
                last = Some(now);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    // Offer the focus to somebody else's window, the way a window manager
    // does, with a real timestamp rather than `CurrentTime`. ICCCM says a
    // client may only act on a real one — which is the difference this is here
    // to measure, since smithay sends `CurrentTime` and a game that drops the
    // message sits there deaf with the focus it was never told it had.
    if let Some(target) = std::env::args()
        .skip_while(|arg| arg != "--wake")
        .nth(1)
        .and_then(|arg| {
            let arg = arg.trim_start_matches("0x");
            u32::from_str_radix(arg, 16).ok()
        })
    {
        let (conn, screen_num) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen_num].root;
        let wm_protocols = conn.intern_atom(false, b"WM_PROTOCOLS")?.reply()?.atom;
        let wm_take_focus = conn.intern_atom(false, b"WM_TAKE_FOCUS")?.reply()?.atom;
        // A timestamp the server has actually issued, which is the only kind
        // ICCCM allows here: ask for one by changing a property on the root and
        // reading the time off the notification it sends back.
        let marker = conn.intern_atom(false, b"LXB_PROBE_TIME")?.reply()?.atom;
        conn.change_window_attributes(
            root,
            &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                .event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        conn.change_property8(
            PropMode::APPEND,
            root,
            marker,
            u32::from(AtomEnum::STRING),
            b"",
        )?;
        conn.flush()?;
        let mut time = x11rb::CURRENT_TIME;
        while let Ok(event) = conn.wait_for_event() {
            if let Event::PropertyNotify(event) = event {
                if event.atom == marker {
                    time = event.time;
                    break;
                }
            }
        }
        let message = x11rb::protocol::xproto::ClientMessageEvent::new(
            32,
            target,
            wm_protocols,
            [wm_take_focus, time, 0, 0, 0],
        );
        conn.send_event(false, target, EventMask::NO_EVENT, message)?
            .check()?;
        conn.flush()?;
        println!("offered the focus to {target:#x} with time={time}");
        return Ok(());
    }

    // The other half of what a window manager says when it activates a window:
    // `_NET_ACTIVE_WINDOW` on the root. A client that trusts the property
    // rather than the focus has no other way to be told.
    if let Some(target) = argument("--active") {
        let (conn, screen_num) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen_num].root;
        let net_active = conn
            .intern_atom(false, b"_NET_ACTIVE_WINDOW")?
            .reply()?
            .atom;
        conn.change_property32(
            PropMode::REPLACE,
            root,
            net_active,
            u32::from(AtomEnum::WINDOW),
            &[target],
        )?;
        conn.flush()?;
        println!("said {target:#x} is the active window");
        return Ok(());
    }

    // And the focus itself, taken away and given back with a real timestamp:
    // what a window that ignores a focus it already has would need in order to
    // see an edge at all.
    if let Some(target) = argument("--refocus") {
        let (conn, screen_num) = x11rb::connect(None)?;
        let _ = screen_num;
        conn.set_input_focus(InputFocus::NONE, x11rb::NONE, x11rb::CURRENT_TIME)?
            .check()?;
        conn.flush()?;
        std::thread::sleep(std::time::Duration::from_millis(120));
        conn.set_input_focus(InputFocus::PARENT, target, x11rb::CURRENT_TIME)?
            .check()?;
        conn.flush()?;
        println!("took the focus away from {target:#x} and gave it back");
        return Ok(());
    }

    let take_focus = !std::env::args().any(|arg| arg == "--no-take-focus");
    // Whether the message is answered, which is a separate thing from listing
    // it. Smithay sends `WM_TAKE_FOCUS` stamped `CurrentTime`, and ICCCM says a
    // client may only act on a real timestamp — so a client that follows the
    // specification drops it and never takes the focus it was offered. That is
    // not hypothetical: the game this probe was written for does exactly that,
    // and the compositor's own log says so ("no X11 window held the keyboard").
    let answer_take_focus = !std::env::args().any(|arg| arg == "--ignore-take-focus");

    let (conn, screen_num) = x11rb::connect(None)?;
    let screen = &conn.setup().roots[screen_num];
    let window = conn.generate_id()?;
    let (width, height) = (screen.width_in_pixels, screen.height_in_pixels);

    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        window,
        screen.root,
        0,
        0,
        width,
        height,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new()
            .background_pixel(screen.black_pixel)
            .event_mask(
                EventMask::FOCUS_CHANGE
                    | EventMask::KEY_PRESS
                    | EventMask::KEY_RELEASE
                    | EventMask::BUTTON_PRESS
                    | EventMask::BUTTON_RELEASE
                    | EventMask::STRUCTURE_NOTIFY
                    | EventMask::EXPOSURE,
            ),
    )?;

    let atom = |name: &str| -> Result<u32, Box<dyn std::error::Error>> {
        Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
    };
    let wm_protocols = atom("WM_PROTOCOLS")?;
    let wm_take_focus = atom("WM_TAKE_FOCUS")?;
    let wm_delete_window = atom("WM_DELETE_WINDOW")?;
    let net_wm_name = atom("_NET_WM_NAME")?;
    let utf8_string = atom("UTF8_STRING")?;
    let net_wm_pid = atom("_NET_WM_PID")?;
    // The predefined atoms, as plain numbers: every property request below is
    // generic over the atom type, and an enum that converts to three widths
    // leaves it with nothing to infer from.
    let (wm_name, string, wm_class, cardinal, atom_type, wm_hints) = (
        u32::from(AtomEnum::WM_NAME),
        u32::from(AtomEnum::STRING),
        u32::from(AtomEnum::WM_CLASS),
        u32::from(AtomEnum::CARDINAL),
        u32::from(AtomEnum::ATOM),
        u32::from(AtomEnum::WM_HINTS),
    );

    conn.change_property8(
        PropMode::REPLACE,
        window,
        wm_name,
        string,
        b"probe-x11-input",
    )?;
    conn.change_property8(
        PropMode::REPLACE,
        window,
        net_wm_name,
        utf8_string,
        b"probe-x11-input",
    )?;
    // Two strings, each terminated: the instance and the class, exactly as a
    // Steam game's is written.
    conn.change_property8(
        PropMode::REPLACE,
        window,
        wm_class,
        string,
        b"probe_x11_input\0probe_x11_input\0",
    )?;
    conn.change_property32(
        PropMode::REPLACE,
        window,
        net_wm_pid,
        cardinal,
        &[std::process::id()],
    )?;

    // The protocols a Wine window lists. `WM_TAKE_FOCUS` is the half that
    // makes the globally active model legible to a window manager; without it
    // `input = False` alone means the window wants no keyboard at all.
    let protocols: Vec<u32> = if take_focus {
        vec![wm_take_focus, wm_delete_window]
    } else {
        vec![wm_delete_window]
    };
    conn.change_property32(
        PropMode::REPLACE,
        window,
        wm_protocols,
        atom_type,
        &protocols,
    )?;

    // WM_HINTS, written by hand because `input` is the field this whole probe
    // is about: flags = InputHint | StateHint, input = False, state = Normal.
    const INPUT_HINT: u32 = 1;
    const STATE_HINT: u32 = 2;
    const NORMAL_STATE: u32 = 1;
    let hints: [u32; 9] = [
        INPUT_HINT | STATE_HINT,
        0, // input = False
        NORMAL_STATE,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    conn.change_property32(PropMode::REPLACE, window, wm_hints, wm_hints, &hints)?;

    conn.map_window(window)?;
    conn.flush()?;

    let started = Instant::now();
    let say = |what: &str| {
        println!("{:7.3}s {what}", started.elapsed().as_secs_f32());
        use std::io::Write;
        let _ = std::io::stdout().flush();
    };
    say(&format!(
        "mapped window={window} size={width}x{height} take_focus={take_focus} \
         answer_take_focus={answer_take_focus}"
    ));

    loop {
        let event = conn.wait_for_event()?;
        match event {
            Event::FocusIn(event) => say(&format!(
                "FocusIn        detail={:?} mode={:?}",
                event.detail, event.mode
            )),
            Event::FocusOut(event) => say(&format!(
                "FocusOut       detail={:?} mode={:?}",
                event.detail, event.mode
            )),
            Event::KeyPress(event) => say(&format!("KeyPress       keycode={}", event.detail)),
            Event::KeyRelease(event) => say(&format!("KeyRelease     keycode={}", event.detail)),
            Event::ButtonPress(event) => say(&format!(
                "ButtonPress    button={} at=({},{})",
                event.detail, event.event_x, event.event_y
            )),
            Event::ButtonRelease(event) => say(&format!("ButtonRelease  button={}", event.detail)),
            Event::ConfigureNotify(event) => say(&format!(
                "ConfigureNotify {}x{} at=({},{})",
                event.width, event.height, event.x, event.y
            )),
            Event::ClientMessage(event) => {
                let data = event.data.as_data32();
                if event.type_ == wm_protocols && data[0] == wm_take_focus {
                    say(&format!("WM_TAKE_FOCUS  time={}", data[1]));
                    if !answer_take_focus {
                        say("ignored it     (ICCCM: the timestamp is CurrentTime)");
                        continue;
                    }
                    // What Wine does with it, and the whole point of the
                    // globally active model: the client sets its own focus.
                    // Checked, because X answers a refused focus request
                    // asynchronously and an unchecked one comes back `Ok` from
                    // a connection that is about to throw the error away.
                    if let Err(err) = conn
                        .set_input_focus(InputFocus::PARENT, window, data[1])
                        .map_err(|err| err.to_string())
                        .and_then(|cookie| cookie.check().map_err(|err| err.to_string()))
                    {
                        say(&format!("could not take the focus: {err}"));
                    }
                    conn.flush()?;
                    let focus = conn.get_input_focus()?.reply()?.focus;
                    say(&format!("took the focus  x_focus={focus}"));
                } else if event.type_ == wm_protocols && data[0] == wm_delete_window {
                    say("WM_DELETE_WINDOW");
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}
