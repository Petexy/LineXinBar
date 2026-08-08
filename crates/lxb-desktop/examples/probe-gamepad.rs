//! What GilRs makes of a pad's face buttons, and what the shell makes of that.
//!
//! Scratch diagnostic, for the case where one button on one controller does
//! nothing. The shell reads pads through GilRs, and GilRs names the same
//! physical button two different ways depending on whether it found an SDL
//! mapping for the pad: a mapped pad is named by *layout*, so `North` really is
//! the button above the others, and an unmapped one is named from the kernel's
//! *positional* aliases for the raw codes, under which `0x133` is `BTN_NORTH`
//! even on the drivers that emit it for the button with `X` printed on it.
//!
//! So the one thing worth printing is both halves at once: which name GilRs
//! gave a button, which raw code the kernel sent for it, and — the part that
//! settles it — whether the naming came from a mapping or from that fallback.
//!
//! It prints the layout first, without a button being touched: GilRs will say
//! which code it has associated with each of the four face buttons. Then it
//! streams presses, so the printed layout can be checked against the button
//! actually under the thumb.
//!
//! Run it from the session that owns the seat. The ACLs on `/dev/input` follow
//! the *active* seat, so a pad missing from the list may be one this terminal
//! cannot open rather than one GilRs refused.

use std::time::{Duration, Instant};

use gilrs::{Button, EventType, GilrsBuilder, MappingSource};

const WINDOW: Duration = Duration::from_secs(45);

/// Linux input codes, packed the way GilRs reports them: `(EV_KEY << 16) | code`.
const fn key(code: u32) -> u32 {
    (0x01 << 16) | code
}

/// The two adjacent codes the face buttons quarrel over. `0x133` is `BTN_X` to
/// the kernel's legacy gamepad names and `BTN_NORTH` to its positional ones;
/// `0x134` is `BTN_Y` and `BTN_WEST`. Which physical button each is depends on
/// the driver, which is the whole reason this probe exists.
const BTN_X: u32 = key(0x133);
const BTN_WEST: u32 = key(0x134);
/// The third and fourth buttons on a pad numbered as a plain joystick, where
/// the other two face buttons sit.
const BTN_THUMB2: u32 = key(0x122);
const BTN_TOP: u32 = key(0x123);

fn code_name(code: u32) -> &'static str {
    match code {
        c if c == key(0x130) => "BTN_SOUTH/BTN_A",
        c if c == key(0x131) => "BTN_EAST/BTN_B",
        c if c == key(0x132) => "BTN_C",
        c if c == BTN_X => "BTN_X/BTN_NORTH",
        c if c == BTN_WEST => "BTN_Y/BTN_WEST",
        c if c == key(0x135) => "BTN_Z",
        c if c == key(0x136) => "BTN_TL",
        c if c == key(0x137) => "BTN_TR",
        c if c == key(0x138) => "BTN_TL2",
        c if c == key(0x139) => "BTN_TR2",
        c if c == key(0x13a) => "BTN_SELECT",
        c if c == key(0x13b) => "BTN_START",
        c if c == key(0x13c) => "BTN_MODE",
        c if c == key(0x13d) => "BTN_THUMBL",
        c if c == key(0x13e) => "BTN_THUMBR",
        c if c == key(0x120) => "BTN_TRIGGER",
        c if c == key(0x121) => "BTN_THUMB",
        c if c == key(0x122) => "BTN_THUMB2",
        c if c == key(0x123) => "BTN_TOP",
        c if c == key(0x124) => "BTN_TOP2",
        c if c == key(0x125) => "BTN_PINKIE",
        c if c == key(0x126) => "BTN_BASE",
        c if c == key(0x127) => "BTN_BASE2",
        c if c == key(0x128) => "BTN_BASE3",
        c if c == key(0x129) => "BTN_BASE4",
        c if c == key(0x220) => "BTN_DPAD_UP",
        c if c == key(0x221) => "BTN_DPAD_DOWN",
        c if c == key(0x222) => "BTN_DPAD_LEFT",
        c if c == key(0x223) => "BTN_DPAD_RIGHT",
        _ => "?",
    }
}

/// The shell's rules, copied here rather than shared: this crate is a binary,
/// so an example cannot call into it. Kept verbatim so the verdict printed
/// beside each press is the one the shell would reach.
fn is_middle_face(button: Button, code: u32) -> bool {
    match button {
        Button::North | Button::West => matches!(code, BTN_X | BTN_WEST),
        Button::Unknown => matches!(code, BTN_X | BTN_WEST | BTN_THUMB2 | BTN_TOP),
        _ => false,
    }
}

fn is_left_face(button: Button, code: u32, mapped: bool) -> bool {
    if mapped {
        matches!(button, Button::West)
    } else {
        is_middle_face(button, code)
    }
}

fn is_top_face(button: Button, code: u32, mapped: bool) -> bool {
    if mapped {
        matches!(button, Button::North)
    } else {
        is_middle_face(button, code)
    }
}

fn verdict(button: Button, code: u32, mapped: bool) -> &'static str {
    if is_top_face(button, code, mapped) && is_left_face(button, code, mapped) {
        "MIDDLE FACE -> context menu, or the keyboard chord with Select"
    } else if is_top_face(button, code, mapped) {
        "TOP FACE -> context menu"
    } else if is_left_face(button, code, mapped) {
        "left face -> keyboard chord (with Select)"
    } else {
        match button {
            Button::South => "A -> Launch",
            Button::East => "B -> Back",
            Button::Mode => "Guide",
            Button::Start => "Start -> Launch",
            Button::Select => "Select (chord modifier only)",
            Button::LeftTrigger => "L1 -> previous display",
            Button::RightTrigger => "R1 -> next display",
            _ => "nothing",
        }
    }
}

fn main() {
    let mut gilrs = match GilrsBuilder::new().with_force_feedback(false).build() {
        Ok(gilrs) => gilrs,
        Err(err) => {
            println!("GilRs would not start: {err}");
            return;
        }
    };

    println!("# what GilRs enumerated\n");
    let mut pads = 0;
    for (id, gamepad) in gilrs.gamepads() {
        pads += 1;
        println!("  [{id}] {}", gamepad.name());
        println!("        uuid    {}", uuid::Uuid(gamepad.uuid()));
        // The line the whole question turns on. `SdlMappings` means the names
        // below describe the physical layout; anything else means they were
        // guessed from the raw codes.
        println!("        mapping {:?}", gamepad.mapping_source());
        println!("        power   {:?}", gamepad.power_info());
        println!("        which code GilRs has associated with each face button:");
        for button in [
            Button::South,
            Button::East,
            Button::West,
            Button::North,
            Button::Select,
            Button::Start,
            Button::Mode,
            Button::LeftTrigger,
            Button::RightTrigger,
        ] {
            match gamepad.button_code(button) {
                Some(code) => {
                    let raw = code.into_u32();
                    println!(
                        "          {:<14} {:#08x}  {}",
                        format!("{button:?}"),
                        raw,
                        code_name(raw)
                    );
                }
                None => println!("          {:<14} —", format!("{button:?}")),
            }
        }
        println!();
    }

    if pads == 0 {
        println!(
            "  nothing.\n\n\
             # No pad has a gamepad node this process can open. Switch the\n\
             # controller on, and check the ACLs: /dev/input follows the active\n\
             # seat, so another session's terminal sees almost none of it."
        );
        return;
    }

    println!(
        ">>> PRESS EVERY FACE BUTTON IN TURN for {} seconds — A, B, the left-hand one, the top one\n",
        WINDOW.as_secs()
    );

    let end = Instant::now() + WINDOW;
    while Instant::now() < end {
        while let Some(event) = gilrs.next_event() {
            let EventType::ButtonPressed(button, code) = event.event else {
                continue;
            };
            let raw = code.into_u32();
            let mapped = gilrs.gamepad(event.id).mapping_source() == MappingSource::SdlMappings;
            println!(
                "  {:<14} code={:#08x} {:<16}  {}",
                format!("{button:?}"),
                raw,
                code_name(raw),
                verdict(button, raw, mapped)
            );
        }
        std::thread::sleep(Duration::from_millis(4));
    }
}

/// GilRs reports a pad's UUID as raw bytes; printed here in the shape SDL's
/// mapping database writes them, so an entry can be looked up by eye.
mod uuid {
    pub struct Uuid(pub [u8; 16]);

    impl std::fmt::Display for Uuid {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            for byte in self.0 {
                write!(f, "{byte:02x}")?;
            }
            Ok(())
        }
    }
}
