//! The on-screen keyboard: what it looks like, and how it types.
//!
//! A console has no keyboard, so the shell has to be one. Three standard
//! protocols carry that, and it is worth being clear about which does what,
//! because the division is not obvious:
//!
//! * `zwp_text_input_v3` is the application's side. A toolkit sends it when a
//!   text field takes the cursor. The shell never sees it.
//! * `zwp_input_method_v2` is the shell's side of the same conversation. Its
//!   `activate` event is the *only* signal Wayland offers that a keyboard
//!   should come up: nothing else in the protocol describes what is inside a
//!   window, so without it there is no such thing as noticing a search box.
//! * `zwp_virtual_keyboard_v1` is how the shell then types. It uploads a
//!   keymap of its own making and sends keycodes through the seat.
//!
//! Typing through the virtual keyboard rather than through the input method's
//! own `commit_string` is deliberate. Keycodes go to whatever holds the
//! keyboard — a terminal, a game, an X11 window forwarded through Xwayland —
//! and not only to clients that speak text-input. The input method is used
//! purely as the doorbell.
//!
//! The consequence, which is the honest limitation of every Wayland keyboard:
//! an application that never sends `zwp_text_input_v3` — most Chromium builds
//! without `--enable-wayland-ime`, and every X11 client — has no way to say a
//! field is focused, so the keyboard cannot come up by itself there. It can
//! still be summoned, and typing into it still works, which is exactly why
//! the manual shortcut exists rather than being a convenience.
//!
//! The keyboard never takes keyboard focus. It cannot: the shell holding the
//! keys would take them from the field being typed into, the application's
//! text input would deactivate, and the keyboard would put itself away. It is
//! driven from the controller, which the shell reads straight from
//! `/dev/input`.
//!
//! It does borrow the physical keyboard while it is up, through the input
//! method's own grab, and that is there for one purpose: to find out that the
//! user has one. A key pressed on a real keyboard is proof that the board is
//! not needed — nobody hunts for letters with a stick while a keyboard is
//! under their hands — so the board puts itself away and passes the key on, as
//! though the matching key on it had been pressed. Nothing on the board is
//! navigable from a keyboard, deliberately: the arrows would be moving a
//! cursor around a picture of the keys the user is already typing on.

use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Mutex;

use smithay_client_toolkit::seat::keyboard::Keysym;
use wayland_client::globals::{BindError, GlobalList};
use wayland_client::protocol::wl_keyboard::{KeyState, KeymapFormat};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_keyboard_grab_v2::{
    self, ZwpInputMethodKeyboardGrabV2,
};
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_manager_v2::ZwpInputMethodManagerV2;
use wayland_protocols_misc::zwp_input_method_v2::client::zwp_input_method_v2::{
    self, ZwpInputMethodV2,
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1;
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1;

use xkbcommon::xkb;

use crate::guide::Move;
use crate::icons;
use crate::Shell;

/// The character rows a board with no layout to read shows: what the key
/// types, and what Shift makes of it.
///
/// The ANSI arrangement and a US layout, right down to where the backslash sits
/// and which row the backtick starts. It is the **fallback** and not the board:
/// the caps follow whatever the session's keyboards are set to, read off that
/// layout's own keymap — see [`note_layout`] — and this is what is shown until
/// something says what that is, on a compositor too old to say, and if a
/// keymap will not compile.
///
/// What does *not* follow the layout is where the keys are. The arrangement
/// stays ANSI whatever is printed on it, which is the one thing a board driven
/// with a thumb cannot afford to move: the user is hunting for a letter by
/// looking, and a grid that changed shape between layouts would be a different
/// board each time. So a French layout puts A where ANSI prints Q — because
/// that is what xkb maps that position to — and the position itself does not
/// move.
///
/// The one key an ANSI board does not have is ISO's extra one beside the left
/// Shift, which carries `<` and `>` on most European layouts. Nothing can be
/// done about that here: it is a key this board does not have, exactly as it is
/// a key an American keyboard does not have.
const NUMBER_ROW: (&str, &str) = ("`1234567890-=", "~!@#$%^&*()_+");
const UPPER_ROW: (&str, &str) = ("qwertyuiop[]", "QWERTYUIOP{}");
const HOME_ROW: (&str, &str) = ("asdfghjkl;'", "ASDFGHJKL:\"");
const LOWER_ROW: (&str, &str) = ("zxcvbnm,./", "ZXCVBNM<>?");

/// The X11 keycode of every character key on the board, by row.
///
/// This is what makes the caps follow the layout: a keymap answers "what does
/// this key produce" about a *keycode*, so the board's ANSI positions have to
/// be named in the only language xkb has for them. X11's numbering, which is
/// evdev's plus eight — `<AE01>`, the key printed 1, is `KEY_1` (2) plus eight.
///
/// Four rows, in the order [`row_spans`] lays them out and with exactly the
/// number of keys that row draws: thirteen across the numbers, twelve letters
/// and the backslash on the upper row, eleven on the home row and ten on the
/// lower. The counts are fixed here rather than read from anywhere, which is
/// what stops a layout changing the width of a row — every row but the function
/// row comes to exactly [`COLUMNS`], and a keymap is not allowed a say in that.
const KEYCODES: [&[u32]; 4] = [
    // <TLDE> and <AE01>..<AE12>
    &[49, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21],
    // <AD01>..<AD12>, then <BKSL> — which the row draws last and wider.
    &[24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 51],
    // <AC01>..<AC11>
    &[38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48],
    // <AB01>..<AB10>
    &[52, 53, 54, 55, 56, 57, 58, 59, 60, 61],
];

/// What the session's keyboards are set to, as caps this board can print.
///
/// `None` until something says — a compositor too old to report its layout
/// leaves it here for the whole session, and so does a keymap that would not
/// compile. Both fall back to the ANSI/US rows above, which is the arrangement
/// this board had before it followed anything.
///
/// A static rather than a field on [`Board`], because the board is built fresh
/// whenever the keyboard comes up and this is a fact about the session: reading
/// a keymap is a file opened and a grammar parsed, and doing it per board would
/// be doing it every time somebody touched a text field.
static CAPS: Mutex<Option<Arrangement>> = Mutex::new(None);

/// The caps of the four character rows, in [`KEYCODES`] order.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Arrangement {
    rows: [Vec<Cap>; 4],
    /// Whether anything on it is reached with AltGr, which is what decides
    /// whether the board draws that key at all. A US board has no AltGr, and
    /// one that drew a dead key in the bottom row would be a key that does
    /// nothing on the layout most people are using.
    altgr: bool,
}

/// Read what a layout puts on the board, and keep it. `true` when the board has
/// to be redrawn, which is whenever it changed.
///
/// Called with whatever the compositor says the seat is set to — including the
/// layout the shell never chose, which is the one in that compositor's own
/// config file. See `Shell::sync_keyboard_layout`.
pub fn note_layout(layout: &str, variant: &str) -> bool {
    let read = read_arrangement(layout, variant);
    if read.is_none() {
        tracing::warn!(
            layout,
            variant,
            "that keyboard layout would not compile; the board keeps the US arrangement"
        );
    }
    let mut held = CAPS.lock().unwrap();
    if *held == read {
        return false;
    }
    *held = read;
    true
}

/// Compile a layout and read the four character rows off it.
///
/// The four faces the board can show are xkb's first four shift levels, which
/// is what a keyboard's four-level type *is*: plain, Shift, AltGr, and both.
/// Levels beyond those exist on a handful of layouts and are not reachable
/// here — this board has one modifier key for the third and fourth, and no
/// keycap says what a fifth would be.
///
/// A key with nothing on a level gets nothing, rather than falling back to its
/// plain character. A cap that showed `a` on the AltGr face and typed `a` when
/// pressed would be a key that ignores the modifier the user is holding, and
/// there would be no way to tell it from one that does something.
fn read_arrangement(layout: &str, variant: &str) -> Option<Arrangement> {
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    // The names as given and nothing else: rules, model and options are the
    // compositor's to decide, and this only has to agree with it about which
    // symbols the keys carry.
    let keymap = xkb::Keymap::new_from_names(
        &context,
        "",
        "",
        layout,
        variant,
        None,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )?;
    let mut rows: [Vec<Cap>; 4] = Default::default();
    let mut altgr = false;
    for (row, keycodes) in KEYCODES.iter().enumerate() {
        for keycode in *keycodes {
            let cap = cap_of(&keymap, *keycode);
            altgr |= cap.has_altgr();
            rows[row].push(cap);
        }
    }
    // A keymap that compiled but says nothing about the alphabet is not an
    // arrangement — it is a layout this board cannot show, and the US fallback
    // is a better board than one with a blank home row.
    rows[2]
        .iter()
        .any(|cap| cap.at(Level::Plain).is_some())
        .then_some(Arrangement { rows, altgr })
}

/// What one key of the keymap types on each of the board's four faces.
fn cap_of(keymap: &xkb::Keymap, keycode: u32) -> Cap {
    let key = xkb::Keycode::new(keycode);
    let mut levels = [None; 4];
    for (index, slot) in levels.iter_mut().enumerate() {
        // One keysym or none. A level bound to several — which xkb allows and
        // almost nothing uses — is one keycap's worth of typing here, so the
        // first is what the key says and what it sends.
        *slot = keymap
            .key_get_syms_by_level(key, 0, index as u32)
            .first()
            .copied()
            .and_then(stroke_of);
    }
    Cap { levels }
}

/// One keysym as this board would type it.
///
/// A character where it has one — which is nearly all of them, and is what lets
/// a layout grow an accented letter without a line of code — and the keysym
/// itself where it has not. The second is the dead keys: `dead_acute` types no
/// character, it changes what the *next* key types, and the honest thing for a
/// board to do with it is send exactly what the key on the desk sends.
fn stroke_of(keysym: Keysym) -> Option<Stroke> {
    if let Some(character) =
        char::from_u32(xkb::keysym_to_utf32(keysym)).filter(|c| !c.is_control())
    {
        return Some(Stroke::Char(character));
    }
    // Anything else with no character of its own is not a thing a keycap can
    // say: NoSymbol, and the handful of layouts that put a function key in the
    // middle of the alphabet.
    dead_mark(keysym)
        .is_some()
        .then_some(Stroke::Keysym(keysym.raw()))
}

/// The accent printed on a dead key's cap.
///
/// A dead key has no character, so this is the one place the board cannot ask
/// the keymap what to print. What a real keycap shows is the accent itself, and
/// that is what this is: the spacing form of each `dead_` keysym
/// xkeyboard-config puts on the alphabet of a European layout.
///
/// A dead key this table does not know is left off the board rather than shown
/// blank — [`stroke_of`] answers `None` for it — because a cap with nothing on
/// it is a key nobody can find out the meaning of by pressing.
fn dead_mark(keysym: Keysym) -> Option<&'static str> {
    let name = xkb::keysym_get_name(keysym);
    Some(match name.strip_prefix("dead_")? {
        "grave" => "`",
        "acute" => "´",
        "circumflex" => "^",
        "tilde" | "perispomeni" => "~",
        "macron" => "¯",
        "breve" => "˘",
        "abovedot" => "˙",
        "diaeresis" => "¨",
        "abovering" => "˚",
        "doubleacute" => "˝",
        "caron" => "ˇ",
        "cedilla" => "¸",
        "ogonek" => "˛",
        "iota" => "ͅ",
        "belowdot" => "̣",
        "hook" => "̉",
        "horn" => "̛",
        "stroke" => "̶",
        "abovecomma" | "psili" => "᾿",
        "abovereversedcomma" | "dasia" => "῾",
        "doublegrave" => "̏",
        "belowring" => "̥",
        "belowmacron" => "̱",
        "belowcircumflex" => "̭",
        "belowtilde" => "̰",
        "belowbreve" => "̮",
        "belowdiaeresis" => "̤",
        "invertedbreve" => "̑",
        "belowcomma" => "̦",
        "currency" => "¤",
        "greek" => "µ",
        _ => return None,
    })
}

/// The function row. The word printed on the cap and the name xkb knows the
/// key by are the same, so one list serves for both.
const FUNCTION_KEYS: [&str; 12] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
];

/// How many columns wide the grid is.
///
/// ANSI's own width: the main block of a full-size keyboard is fifteen keys
/// across, which is exactly what makes Tab one and a half of them and the
/// right-hand Shift two and three quarters.
pub const COLUMNS: f32 = 15.0;

/// Rows on the board: the function row, the four character rows, and the
/// bottom row carrying the modifiers, the space bar, the arrows and the way
/// out.
pub const ROW_COUNT: usize = 6;

/// Which row the function keys are on.
const FUNCTION_ROW: usize = 0;

/// How tall a row is drawn, as a share of a full keycap.
///
/// The function row is a strip a little over half height, which is what a
/// keyboard that has one looks like, and what keeps thirteen keys nobody
/// reaches for while typing a password from taking a sixth of the board.
pub fn row_scale(row: usize) -> f32 {
    if row == FUNCTION_ROW {
        0.56
    } else {
        1.0
    }
}

/// Where the cursor starts. The home row's first letter, not the top-left
/// function key: it is where a word starts, and it is roughly the middle of
/// the board in both directions.
const HOME: (usize, usize) = (3, 1);

/// Where the keymap's keycodes start. 8 is the X11 offset every xkb keymap is
/// written against, and 8 itself is evdev's reserved code, so the first key
/// the shell can actually send is the one after it.
const FIRST_KEYCODE: u32 = 9;

/// An arrow key.
///
/// Its own variant rather than one more [`Key::Named`], because an arrow is
/// the one key on the board that cannot be lettered: the shell ships Roboto
/// precisely so it does not depend on what fonts a console happens to have,
/// and Roboto has no arrow glyphs. So the shell draws them, from four of its
/// own SVGs, the same way the corner hint draws the controller buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrow {
    Left,
    Down,
    Up,
    Right,
}

impl Arrow {
    /// Left to right along the bottom row, in the order a keyboard's arrow
    /// cluster reads.
    const ALL: [Arrow; 4] = [Arrow::Left, Arrow::Down, Arrow::Up, Arrow::Right];

    /// What it types.
    pub fn stroke(self) -> Stroke {
        Stroke::Named(match self {
            Arrow::Left => "Left",
            Arrow::Down => "Down",
            Arrow::Up => "Up",
            Arrow::Right => "Right",
        })
    }

    /// The shell's own drawing of it.
    pub fn glyph(self) -> &'static str {
        match self {
            Arrow::Left => icons::ARROW_LEFT,
            Arrow::Down => icons::ARROW_DOWN,
            Arrow::Up => icons::ARROW_UP,
            Arrow::Right => icons::ARROW_RIGHT,
        }
    }
}

/// One key on the board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A character key: what it types on each of the four faces the board can
    /// show. See [`Cap`].
    Char(Cap),
    /// A key that types one fixed thing whatever the shift state, and the word
    /// printed on it: Esc, Tab, Enter, Back, Space, and the twelve function
    /// keys.
    Named(&'static str, Stroke),
    /// An arrow key, drawn rather than lettered.
    Arrow(Arrow),
    /// Shift: armed for one character, and locking on a second press.
    Shift,
    /// Caps Lock, where ANSI puts it — the lock on its own, so a run of
    /// capitals need not go through Shift's one-shot state first.
    Caps,
    /// Ctrl and Alt, which latch the same way Shift does. They change nothing
    /// about what the caps say — they are sent alongside the next key, so that
    /// a board can reach Ctrl+C and Alt+F4 as well as the alphabet.
    Ctrl,
    Alt,
    /// AltGr: the third and fourth faces of the board, where most layouts keep
    /// their accented letters and their currency signs.
    ///
    /// It latches like Shift and, like Shift, is **not** sent as a modifier:
    /// the board's own keymap gives every character its own key, so `ą` is
    /// reached by sending `ą` and nothing can be left held down when the board
    /// goes away. See [`Board::modifiers`].
    ///
    /// On the board only where the layout has something on those faces. A US
    /// keyboard has no AltGr, and a key that did nothing on the layout most
    /// people use would be a key nobody could learn the meaning of.
    AltGr,
    /// Not an ANSI key at all, and the one addition to the arrangement: a
    /// keyboard that has appeared by itself has to be dismissable by someone
    /// who did not summon it and does not know what did.
    Close,
}

impl Key {
    /// A character key with a plain and a shifted character and nothing on
    /// AltGr, for naming one in a test without spelling out four levels.
    #[cfg(test)]
    pub const fn letter(plain: char, shifted: char) -> Key {
        Key::Char(Cap::letter(plain, shifted))
    }

    /// What is printed on the key, on the face the board is showing. Empty for
    /// the keys that are drawn instead — see [`Self::glyph`] — and for a
    /// character key with nothing on that face.
    pub fn cap(self, level: Level) -> String {
        match self {
            Key::Char(cap) => cap.printed(level),
            Key::Named(cap, _) => cap.to_string(),
            Key::Arrow(_) | Key::Close => String::new(),
            Key::Shift => "Shift".to_string(),
            Key::Caps => "Caps".to_string(),
            Key::Ctrl => "Ctrl".to_string(),
            Key::Alt => "Alt".to_string(),
            Key::AltGr => "AltGr".to_string(),
        }
    }

    /// The shell's own drawing for this key, where it carries one instead of a
    /// word.
    ///
    /// The arrows, because the bundled font has no arrow glyphs; and the way
    /// out, because a keyboard folding itself away is a picture every phone
    /// has taught, and one that needs no reading at a distance.
    pub fn glyph(self) -> Option<&'static str> {
        match self {
            Key::Arrow(arrow) => Some(arrow.glyph()),
            Key::Close => Some(icons::KEYBOARD_HIDE),
            _ => None,
        }
    }

    /// Whether this key ends the session with the keyboard rather than typing
    /// into it. Drawn apart from the rest, the way the power dialog's two
    /// grave choices are.
    pub fn is_close(self) -> bool {
        self == Key::Close
    }
}

/// Which face of the board is showing.
///
/// xkb's first four shift levels, which is what a keyboard's four-level type
/// is. The board reaches them with two keys: Shift, and AltGr where the layout
/// has anything on the far two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Level {
    #[default]
    Plain,
    Shift,
    AltGr,
    AltGrShift,
}

impl Level {
    fn of(shifted: bool, altgr: bool) -> Level {
        match (altgr, shifted) {
            (false, false) => Level::Plain,
            (false, true) => Level::Shift,
            (true, false) => Level::AltGr,
            (true, true) => Level::AltGrShift,
        }
    }

    fn index(self) -> usize {
        match self {
            Level::Plain => 0,
            Level::Shift => 1,
            Level::AltGr => 2,
            Level::AltGrShift => 3,
        }
    }
}

/// What one character key types, on each face the board can show.
///
/// Four answers rather than the two a keycap is printed with, because a layout
/// keeps its accented letters on the far two: `ą` is AltGr and `a` on a Polish
/// keyboard, and a board offering only the near pair would be a board a Pole
/// could not write their own language on.
///
/// `None` where the layout puts nothing on that face, which is most keys on
/// most layouts. The cap is blank there and the press types nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap {
    levels: [Option<Stroke>; 4],
}

impl Cap {
    /// A key with a plain and a shifted character and nothing on AltGr.
    pub const fn letter(plain: char, shifted: char) -> Cap {
        Cap {
            levels: [
                Some(Stroke::Char(plain)),
                Some(Stroke::Char(shifted)),
                None,
                None,
            ],
        }
    }

    /// What it types on one face, if it types anything there.
    fn at(self, level: Level) -> Option<Stroke> {
        self.levels[level.index()]
    }

    /// What is printed on it on one face.
    fn printed(self, level: Level) -> String {
        match self.at(level) {
            Some(Stroke::Char(character)) => character.to_string(),
            // The accent a dead key carries, which is what the cap on a real
            // keyboard shows: the key types no character, so there is nothing
            // else it could say.
            Some(Stroke::Keysym(raw)) => dead_mark(Keysym::new(raw)).unwrap_or("").to_string(),
            Some(Stroke::Named(name)) => name.to_string(),
            None => String::new(),
        }
    }

    /// Whether the layout puts anything on either AltGr face.
    fn has_altgr(self) -> bool {
        self.levels[2].is_some() || self.levels[3].is_some()
    }

    /// Everything this key can send, for the keymap to give each of them a key.
    fn strokes(self) -> impl Iterator<Item = Stroke> {
        self.levels.into_iter().flatten()
    }
}

/// One key's worth of typing, as the keymap knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stroke {
    Char(char),
    /// A key xkb knows by name rather than by the character it produces:
    /// Return, Tab, the function keys, the arrows.
    Named(&'static str),
    /// A keysym with no character of its own that the *layout* put on the
    /// alphabet: the dead keys, which is how a French or a German keyboard
    /// reaches its accented letters.
    ///
    /// The raw keysym rather than its name, so this stays `Copy` and one word
    /// wide like the two above it. The name is what the keymap is written with
    /// and is looked up when it is written — see [`Stroke::keysym`].
    ///
    /// Passed on exactly as the key on the desk sends it, which means it
    /// composes in the application if the application composes. It does **not**
    /// compose in this shell's own fields: those take the character out of the
    /// stroke and a dead key has none, so a dead key pressed into a search box
    /// types nothing. That is the honest answer — the alternative is a shell
    /// with a compose table of its own disagreeing with every client's.
    Keysym(u32),
}

impl Stroke {
    pub const BACKSPACE: Self = Self::Named("BackSpace");
    pub const ENTER: Self = Self::Named("Return");
    pub const TAB: Self = Self::Named("Tab");
    pub const ESCAPE: Self = Self::Named("Escape");
    pub const SPACE: Self = Self::Char(' ');

    /// The name this stroke is written under in an xkb keymap.
    ///
    /// Characters go in as Unicode keysyms rather than by their traditional
    /// names, which spares the shell a table mapping `!` to `exclam` and `~`
    /// to `asciitilde`, and means a layout that grows an accented letter needs
    /// no new code at all. The named keys have no character to be spelt that
    /// way, so they carry the keysym xkb already has for them.
    fn keysym(self) -> String {
        match self {
            Stroke::Named(name) => name.to_string(),
            Stroke::Char(character) => format!("U{:04X}", character as u32),
            // xkbcommon's own canonical name, which is what it will parse back:
            // there is no Unicode form for a keysym that is not a character.
            Stroke::Keysym(raw) => xkb::keysym_get_name(Keysym::new(raw)),
        }
    }
}

/// What pressing a key did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// Send this through the virtual keyboard.
    Type(Stroke),
    /// Shift changed. Nothing was typed, and the caps all change.
    Shifted,
    /// The key has nothing on the face the board is showing, so the press did
    /// nothing at all — not even change what the next one will do.
    ///
    /// Its own answer rather than typing the plain character, which is what a
    /// key that ignored the modifier the user was holding would do: on the
    /// AltGr face most keys of most layouts are blank, and a blank cap that
    /// typed a letter would be the one thing on this board that lies.
    Nothing,
    /// Put the keyboard away.
    Close,
}

/// A modifier the board holds down on the user's behalf.
///
/// One-shot by default and locking when pressed twice, the way every touch
/// keyboard behaves: one capital is the common case, and a run of them should
/// not be ten presses of the same key. Ctrl and Alt work the same way, because
/// a board driven one key at a time cannot hold anything down — there is only
/// ever one finger on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Latch {
    #[default]
    Off,
    Once,
    Locked,
}

impl Latch {
    /// Where pressing the key takes it: armed, then locked, then off again.
    fn pressed(self) -> Self {
        match self {
            Latch::Off => Latch::Once,
            Latch::Once => Latch::Locked,
            Latch::Locked => Latch::Off,
        }
    }

    /// What an actual keystroke does to it. A latch armed for one key is spent
    /// by it; a locked one is not.
    fn spent(self) -> Self {
        match self {
            Latch::Once => Latch::Off,
            other => other,
        }
    }

    pub fn is_on(self) -> bool {
        self != Latch::Off
    }
}

/// The core X11 modifier masks, which are what a Wayland seat serialises and
/// what every keymap agrees on regardless of what its symbols section says:
/// Shift is bit 0, Lock bit 1, Control bit 2, and Mod1 — Alt — bit 3.
const MOD_CONTROL: u32 = 1 << 2;
const MOD_ALT: u32 = 1 << 3;

/// The keys, and which one the cursor is on.
#[derive(Debug, Default)]
pub struct Board {
    row: usize,
    column: usize,
    shift: Latch,
    ctrl: Latch,
    alt: Latch,
    altgr: Latch,
}

impl Board {
    pub fn selected(&self) -> (usize, usize) {
        (self.row, self.column.min(row_keys(self.row).len() - 1))
    }

    /// Whether Shift is armed or locked. Not what the caps say on its own —
    /// that is [`Self::level`], because AltGr has a say in it too.
    #[cfg(test)]
    pub fn shifted(&self) -> bool {
        self.shift.is_on()
    }

    /// Which face the board is showing, which is what every cap on it says and
    /// what the next press will type.
    pub fn level(&self) -> Level {
        Level::of(self.shift.is_on(), self.altgr.is_on())
    }

    /// How this key is holding the board, if it is one of the keys that can.
    ///
    /// What the board draws from: a latched key stays lit after the cursor has
    /// left it, because it has changed what the next press will do, and a
    /// locked one is lit brighter than one armed for a single key.
    pub fn latched(&self, key: Key) -> Latch {
        match key {
            Key::Shift => self.shift,
            Key::Ctrl => self.ctrl,
            Key::Alt => self.alt,
            Key::AltGr => self.altgr,
            // Caps names the lock and only the lock. A Shift armed for the
            // next letter alone is Shift's business to show.
            Key::Caps if self.shift == Latch::Locked => Latch::Locked,
            _ => Latch::Off,
        }
    }

    /// The modifier mask to hold down around the next keystroke.
    ///
    /// Shift is deliberately not in it, and neither is AltGr. The board's keymap
    /// gives every capital and every accented letter a key of its own, so a
    /// shifted letter is reached by sending `A` rather
    /// than by sending `a` with Shift — which also means nothing can be left
    /// latched in the application if the board goes away mid-word. Ctrl and
    /// Alt have no such key to send, so they go as the mask they are.
    pub fn modifiers(&self) -> u32 {
        let mut mask = 0;
        if self.ctrl.is_on() {
            mask |= MOD_CONTROL;
        }
        if self.alt.is_on() {
            mask |= MOD_ALT;
        }
        mask
    }

    /// Whether this key is holding the board down rather than armed for a
    /// single press.
    #[cfg(test)]
    pub fn locked(&self, key: Key) -> bool {
        self.latched(key) == Latch::Locked
    }

    /// Arm Shift without walking the cursor over to the key, for tests of what
    /// the board *looks* like on its other face.
    #[cfg(test)]
    pub fn arm_shift(&mut self) {
        self.shift = Latch::Once;
    }

    /// Put the cursor on a particular key. Returns whether it went anywhere.
    ///
    /// What the pointer moves. A cursor over the board *is* the selection —
    /// rather than a second highlight of its own — so that the one button that
    /// presses a key presses the key the user is looking at, whether they
    /// walked to it with the D-pad or aimed at it with the stick. Out-of-range
    /// coordinates are refused rather than clamped: they come from a hit test
    /// that found nothing, and the nearest key is not what was pointed at.
    pub fn select(&mut self, row: usize, column: usize) -> bool {
        if row >= ROW_COUNT || column >= row_keys(row).len() {
            return false;
        }
        let moved = (row, column) != self.selected();
        self.row = row;
        self.column = column;
        moved
    }

    /// Move the cursor. Returns whether it went anywhere.
    ///
    /// Left and Right wrap within the row; Up and Down wrap around the board
    /// and land on the key nearest *across* — the rows are not the same length
    /// and their keys are not the same width, so stepping by index would drift
    /// left as it went down the board and put Down from `p` on the Backspace
    /// key.
    pub fn move_selection(&mut self, direction: Move) -> bool {
        let (row, column) = self.selected();
        let (next_row, next_column) = match direction {
            Move::Left => {
                let count = row_keys(row).len();
                (row, (column + count - 1) % count)
            }
            Move::Right => {
                let count = row_keys(row).len();
                (row, (column + 1) % count)
            }
            Move::Up | Move::Down => {
                let step = if direction == Move::Up {
                    ROW_COUNT - 1
                } else {
                    1
                };
                let next_row = (row + step) % ROW_COUNT;
                (next_row, nearest_column(row, column, next_row))
            }
        };
        let moved = (next_row, next_column) != (row, column);
        self.row = next_row;
        self.column = next_column;
        moved
    }

    /// Press the key under the cursor.
    pub fn press(&mut self) -> Press {
        let (row, column) = self.selected();
        let key = row_keys(row)[column];
        match key {
            Key::Shift => {
                self.shift = self.shift.pressed();
                Press::Shifted
            }
            Key::Ctrl => {
                self.ctrl = self.ctrl.pressed();
                Press::Shifted
            }
            Key::Alt => {
                self.alt = self.alt.pressed();
                Press::Shifted
            }
            Key::AltGr => {
                self.altgr = self.altgr.pressed();
                Press::Shifted
            }
            // Caps goes straight to the lock and straight back off it: that is
            // the whole of what the key is for, and having to pass through
            // Shift's armed-for-one state on the way would make it Shift.
            Key::Caps => {
                self.shift = match self.shift {
                    Latch::Locked => Latch::Off,
                    _ => Latch::Locked,
                };
                Press::Shifted
            }
            Key::Close => Press::Close,
            Key::Named(_, stroke) => {
                self.spend();
                Press::Type(stroke)
            }
            Key::Arrow(arrow) => {
                self.spend();
                Press::Type(arrow.stroke())
            }
            Key::Char(cap) => {
                // Read before spending: it is this press the armed shift is
                // for.
                let typed = cap.at(self.level());
                match typed {
                    Some(stroke) => {
                        self.spend();
                        Press::Type(stroke)
                    }
                    // Nothing on this face, so nothing happens — the latches
                    // included. A blank cap that spent the AltGr the user had
                    // just armed would take the modifier away for the key they
                    // were actually reaching for.
                    None => Press::Nothing,
                }
            }
        }
    }

    /// Press Enter without the cursor being on it — the board's half of what
    /// Start does. See [`Osk::submit`].
    ///
    /// It spends the latches exactly as pressing the key itself would: a Ctrl
    /// the user has left armed is armed for the next keystroke, whichever key
    /// sends it. The cursor is deliberately left where it was, because the
    /// board is going away and where it stood is where it should come back.
    pub fn submit(&mut self) -> Press {
        self.spend();
        Press::Type(Stroke::ENTER)
    }

    /// Every latch armed for one key falls away with the key it was armed for.
    /// A locked one does not, which is the whole difference between them.
    fn spend(&mut self) {
        self.shift = self.shift.spent();
        self.ctrl = self.ctrl.spent();
        self.alt = self.alt.spent();
        self.altgr = self.altgr.spent();
    }
}

/// The keys in one row, left to right, with how many grid columns each spans.
///
/// The widths are ANSI's own, in the unit a keyboard is specified in: Tab is
/// one and a half keys, Caps Lock one and three quarters, Enter two and a
/// quarter, the left Shift two and a quarter and the right one two and three
/// quarters. Every row but the function row comes to exactly [`COLUMNS`],
/// which is why the stagger between them looks like a keyboard's rather than
/// like a grid that has been nudged.
///
/// Two departures from ANSI, both because this board is driven one key at a
/// time. The function row is stretched to the full width and drawn at
/// [`row_scale`]'s half height — thirteen keys nobody visits while typing a
/// password should not take a sixth of the board — and the bottom row keeps
/// only the two modifiers that can be reached without a chord, giving the
/// space the rest would have taken to the arrow cluster and the way out.
pub fn row_spans(row: usize) -> Vec<(Key, f32)> {
    let mut keys: Vec<(Key, f32)> = Vec::new();
    match row {
        FUNCTION_ROW => {
            // Thirteen keys sharing the width the other rows use for fifteen,
            // so the strip reaches both edges rather than sitting inset with a
            // gap at each end.
            let span = COLUMNS / 13.0;
            keys.push((Key::Named("Esc", Stroke::ESCAPE), span));
            keys.extend(FUNCTION_KEYS.map(|name| (Key::Named(name, Stroke::Named(name)), span)));
        }
        1 => {
            keys.extend(caps_in(row).into_iter().map(|cap| (Key::Char(cap), 1.0)));
            keys.push((
                Key::Named(crate::i18n::text("shell-back"), Stroke::BACKSPACE),
                2.0,
            ));
        }
        2 => {
            keys.push((Key::Named("Tab", Stroke::TAB), 1.5));
            // The backslash is the row's last key and is drawn wider, which is
            // where ANSI puts it. It is a character key like the twelve before
            // it, so the layout has its say about what it prints — on a German
            // keyboard that position is `#`.
            let caps = caps_in(row);
            let (letters, wide) = caps.split_at(caps.len().saturating_sub(1));
            keys.extend(letters.iter().map(|cap| (Key::Char(*cap), 1.0)));
            keys.extend(wide.iter().map(|cap| (Key::Char(*cap), 1.5)));
        }
        3 => {
            keys.push((Key::Caps, 1.75));
            keys.extend(caps_in(row).into_iter().map(|cap| (Key::Char(cap), 1.0)));
            keys.push((Key::Named("Enter", Stroke::ENTER), 2.25));
        }
        4 => {
            keys.push((Key::Shift, 2.25));
            keys.extend(caps_in(row).into_iter().map(|cap| (Key::Char(cap), 1.0)));
            keys.push((Key::Shift, 2.75));
        }
        _ => {
            keys.push((Key::Ctrl, 1.5));
            keys.push((Key::Alt, 1.5));
            // AltGr takes a key and a half out of the space bar, and only where
            // the layout has something on the faces it reaches. Right of the
            // space bar, which is where a keyboard that has one puts it.
            match altgr_on_the_board() {
                true => {
                    keys.push((Key::Named("Space", Stroke::SPACE), 4.0));
                    keys.push((Key::AltGr, 1.5));
                }
                false => keys.push((Key::Named("Space", Stroke::SPACE), 5.5)),
            }
            keys.extend(Arrow::ALL.map(|arrow| (Key::Arrow(arrow), 1.0)));
            keys.push((Key::Close, 2.5));
        }
    }
    keys
}

/// The caps of one character row: the layout's, or the ANSI/US fallback where
/// nothing has said what the layout is.
///
/// `row` is the board's own row number — 1 to 4 — which is [`KEYCODES`]'s index
/// plus one.
fn caps_in(row: usize) -> Vec<Cap> {
    if let Some(held) = CAPS.lock().unwrap().as_ref() {
        if let Some(caps) = row.checked_sub(1).and_then(|index| held.rows.get(index)) {
            return caps.clone();
        }
    }
    let (plain, shifted) = match row {
        1 => NUMBER_ROW,
        2 => UPPER_ROW,
        3 => HOME_ROW,
        _ => LOWER_ROW,
    };
    let mut caps: Vec<Cap> = plain
        .chars()
        .zip(shifted.chars())
        .map(|(plain, shifted)| Cap::letter(plain, shifted))
        .collect();
    // The backslash, which the fallback rows above do not carry because it is
    // the one character key drawn at a width of its own.
    if row == 2 {
        caps.push(Cap::letter('\\', '|'));
    }
    caps
}

/// Whether the board draws an AltGr key at all.
fn altgr_on_the_board() -> bool {
    CAPS.lock().unwrap().as_ref().is_some_and(|held| held.altgr)
}

/// The keys in one row, left to right.
pub fn row_keys(row: usize) -> Vec<Key> {
    row_spans(row).into_iter().map(|(key, _)| key).collect()
}

/// Where each key in a row starts, and how wide it is, in grid columns.
///
/// The row is centred, which matters only for the function row: it is thirteen
/// keys of one column in a grid of fifteen, and a real keyboard insets it the
/// same way rather than stretching it to the width of the block below.
pub fn row_layout(row: usize) -> Vec<(f32, f32)> {
    let keys = row_spans(row);
    let width: f32 = keys.iter().map(|(_, span)| *span).sum();
    let mut at = (COLUMNS - width) * 0.5;
    keys.into_iter()
        .map(|(_, span)| {
            let start = at;
            at += span;
            (start, span)
        })
        .collect()
}

/// The key in `row` whose middle is nearest the middle of the key currently
/// selected in `from`.
fn nearest_column(from: usize, column: usize, row: usize) -> usize {
    let middle = |row: usize, column: usize| {
        row_layout(row)
            .get(column)
            .map(|(start, span)| start + span * 0.5)
            .unwrap_or(COLUMNS * 0.5)
    };
    let wanted = middle(from, column);
    row_layout(row)
        .iter()
        .enumerate()
        .min_by(|(_, (a, aw)), (_, (b, bw))| {
            let distance = |start: f32, span: f32| (start + span * 0.5 - wanted).abs();
            distance(*a, *aw).total_cmp(&distance(*b, *bw))
        })
        .map(|(index, _)| index)
        .unwrap_or(0)
}

/// Every stroke the board can send, in the order the keymap gives them
/// keycodes.
fn alphabet() -> Vec<Stroke> {
    let mut strokes = Vec::new();
    let mut push = |stroke: Stroke| {
        if !strokes.contains(&stroke) {
            strokes.push(stroke);
        }
    };
    for row in 0..ROW_COUNT {
        for key in row_keys(row) {
            match key {
                // All four faces, so a letter the layout keeps behind AltGr
                // has a key in the keymap to be sent from. The keymap is built
                // whenever the board is, so it costs nothing on a layout that
                // uses two of them.
                Key::Char(cap) => cap.strokes().for_each(&mut push),
                Key::Named(_, stroke) => push(stroke),
                Key::Arrow(arrow) => push(arrow.stroke()),
                // The keys that change what the board does rather than sending
                // anything — the modifiers go as a mask alongside the next
                // key, not as a key of their own — and the one that puts it
                // away.
                Key::Shift | Key::Caps | Key::Ctrl | Key::Alt | Key::AltGr | Key::Close => {}
            }
        }
    }
    strokes
}

/// An xkb keymap in which every stroke the board can send has a key of its
/// own.
///
/// One keysym per keycode and no modifiers at all: the board decides what a
/// key means — Shift picks the capital rather than asking the keymap for it —
/// so there is no shift level to model, and nothing that can be left latched
/// when the keyboard goes away mid-word.
///
/// `spare` adds one key that types nothing. It is not there to be pressed —
/// see [`Typist::rearm`] for what it is for.
fn keymap(alphabet: &[Stroke], spare: bool) -> String {
    let keys = alphabet.len() + usize::from(spare);
    let mut text = String::from("xkb_keymap {\n");
    text.push_str("xkb_keycodes \"lxb\" {\n");
    text.push_str("  minimum = 8;\n");
    text.push_str(&format!("  maximum = {};\n", FIRST_KEYCODE as usize + keys));
    for index in 0..keys {
        text.push_str(&format!(
            "  <K{index}> = {};\n",
            FIRST_KEYCODE as usize + index
        ));
    }
    text.push_str("};\n");
    text.push_str("xkb_types \"lxb\" { include \"complete\" };\n");
    text.push_str("xkb_compat \"lxb\" { include \"complete\" };\n");
    text.push_str("xkb_symbols \"lxb\" {\n");
    for (index, stroke) in alphabet.iter().enumerate() {
        text.push_str(&format!(
            "  key <K{index}> {{ [ {} ] }};\n",
            stroke.keysym()
        ));
    }
    if spare {
        text.push_str(&format!(
            "  key <K{}> {{ [ VoidSymbol ] }};\n",
            alphabet.len()
        ));
    }
    text.push_str("};\n};\n");
    text
}

/// Put the keymap somewhere the compositor can map it.
///
/// A sealed-off anonymous file rather than one under `/tmp`: it exists for the
/// length of one `keymap` request and has no business being on a filesystem.
fn keymap_file(text: &str) -> std::io::Result<(OwnedFd, u32)> {
    // SAFETY: a null-terminated literal name and a flag constant; the fd is
    // owned from the moment it is returned.
    let raw = unsafe { libc::memfd_create(c"lxb-keymap".as_ptr(), libc::MFD_CLOEXEC) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `memfd_create` returned it, and nothing else holds it.
    let mut file = unsafe { <File as std::os::fd::FromRawFd>::from_raw_fd(raw) };
    file.write_all(text.as_bytes())?;
    // libxkbcommon reads the mapping as a C string, so the size a client sends
    // counts the terminator.
    file.write_all(&[0])?;
    file.flush()?;
    Ok((OwnedFd::from(file), text.len() as u32 + 1))
}

/// What a key from the physical keyboard means while the board is up.
///
/// Every one of them ends with the board gone. What differs is only whether
/// the key that dismissed it has somewhere to go afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typed {
    /// Put the board away and type this into the application, exactly as
    /// though the matching key on the board had been pressed.
    Send(Stroke),
    /// Put the board away, and nothing else.
    Close,
    /// Not a key at all — a release, or a press arriving with no grab to have
    /// carried it.
    Ignored,
}

/// Read a key the user pressed on their own keyboard while the board was up.
///
/// One rule, and it is about the keyboard rather than the key: whatever it
/// was, the user has a keyboard, so the board goes away. The key itself is
/// forwarded where the board's keymap can express it, because the first letter
/// of a word is not one the user should have to type twice — and the board is
/// still holding the grab that key arrived on, so if it does not pass it on,
/// nobody does.
///
/// Escape is the one key that dismisses without typing, which is what it has
/// always done here and what it does on every keyboard that appears by itself.
pub fn interpret(keysym: Keysym) -> Typed {
    if keysym == Keysym::Escape {
        return Typed::Close;
    }
    match stroke_for(keysym) {
        Some(stroke) => Typed::Send(stroke),
        // A modifier on its own, a Super key, a volume key: nothing the board
        // has a key for. The hand is still on a keyboard, so the board still
        // goes; the key goes nowhere, which is where it was already headed.
        None => Typed::Close,
    }
}

/// The stroke the board's keymap would use for this keysym, if it has one.
///
/// Public because the shell has fields of its own — the password a removal is
/// authorised with, and the one polkit asks for — and a key pressed on a real
/// keyboard has to reach them by the same route a key on the board does, so
/// that the two cannot disagree about what Backspace or Return mean.
pub fn stroke_for(keysym: Keysym) -> Option<Stroke> {
    // The function keysyms are consecutive, and so is the row.
    let function = keysym.raw().checked_sub(Keysym::F1.raw());
    if let Some(name) = function.and_then(|index| FUNCTION_KEYS.get(index as usize)) {
        return Some(Stroke::Named(name));
    }
    match keysym {
        Keysym::BackSpace => Some(Stroke::BACKSPACE),
        Keysym::Tab | Keysym::ISO_Left_Tab => Some(Stroke::TAB),
        // The keys with no character of their own, each of which the board
        // does have a cap for — so a field being arrowed through, or a form
        // being finished with Return, loses nothing to the board having been
        // in the way.
        Keysym::Return | Keysym::KP_Enter => Some(Stroke::ENTER),
        Keysym::Escape => Some(Stroke::ESCAPE),
        Keysym::Left | Keysym::KP_Left => Some(Arrow::Left.stroke()),
        Keysym::Right | Keysym::KP_Right => Some(Arrow::Right.stroke()),
        Keysym::Up | Keysym::KP_Up => Some(Arrow::Up.stroke()),
        Keysym::Down | Keysym::KP_Down => Some(Arrow::Down.stroke()),
        // Anything with a character of its own goes as that character. The
        // keysym has had the layout and the shift level applied already, so
        // Shift and `a` arrive here as `A` — for which the board's keymap has
        // a key of its own, and no modifier is needed to reach it.
        _ => char::from_u32(xkb::keysym_to_utf32(keysym))
            .filter(|character| !character.is_control())
            .map(Stroke::Char),
    }
}

/// Shift and Caps Lock, which are the two modifiers *not* passed on with a
/// key.
///
/// [`stroke_for`] reads the keysym, which has already had them applied, and
/// the board's keymap gives every character — capital or not — a key of its
/// own. Forwarding the modifier as well would tell the application the letter
/// had been shifted twice. Everything else does go through, which is what
/// keeps Ctrl+C working while the board is on screen.
const LEVEL_MODIFIERS: u32 = 0x1 | 0x2;

/// The modifier state, as the seat serialises it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Modifiers {
    depressed: u32,
    latched: u32,
    locked: u32,
    group: u32,
}

impl Modifiers {
    /// The part worth passing on — see [`LEVEL_MODIFIERS`].
    fn forwarded(self) -> Self {
        Self {
            depressed: self.depressed & !LEVEL_MODIFIERS,
            latched: self.latched & !LEVEL_MODIFIERS,
            locked: self.locked & !LEVEL_MODIFIERS,
            group: self.group,
        }
    }

    fn is_empty(self) -> bool {
        self == Self::default()
    }
}

/// The physical keyboard, borrowed for as long as the board is on screen.
///
/// Not by taking Wayland keyboard focus, which the board must never do:
/// focus is what carries text-input focus, so the moment the shell holds the
/// keys the application's field deactivates and the board puts itself away.
/// `zwp_input_method_v2.grab_keyboard` is the input method's own way in — the
/// compositor routes key events here instead of to the application, and the
/// field the board is typing into does not move at all.
///
/// Held for one key at most. The first press on it is the board's notice to
/// leave, so nothing here has to model a key being held: the grab is gone
/// before the release arrives, and the repeat the compositor offers with it is
/// the application's business from that moment on.
struct Grab {
    keyboard: ZwpInputMethodKeyboardGrabV2,
    /// The *session's* keymap, handed over with the grab: the user's own
    /// layout, which is what says whether the key they pressed was an arrow or
    /// the letter `q`. Nothing to do with the board's own keymap, which
    /// describes the keys the board sends rather than the ones it receives.
    xkb: Option<xkb::State>,
    modifiers: Modifiers,
}

impl Grab {
    fn new(keyboard: ZwpInputMethodKeyboardGrabV2) -> Self {
        Self {
            keyboard,
            xkb: None,
            modifiers: Modifiers::default(),
        }
    }

    /// Compile the keymap the compositor sent with the grab.
    fn load_keymap(&mut self, format: KeymapFormat, fd: OwnedFd, size: u32) {
        if format != KeymapFormat::XkbV1 {
            tracing::warn!(
                ?format,
                "the seat's keymap is in a format the shell cannot read"
            );
            return;
        }
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        // SAFETY: the fd and size are the compositor's own, and the mapping is
        // private and read-only, as the protocol requires of a keymap.
        let keymap = unsafe {
            xkb::Keymap::new_from_fd(
                &context,
                fd,
                size as usize,
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::KEYMAP_COMPILE_NO_FLAGS,
            )
        };
        match keymap {
            Ok(Some(keymap)) => self.xkb = Some(xkb::State::new(&keymap)),
            Ok(None) => tracing::warn!("the seat's keymap would not compile"),
            Err(err) => tracing::warn!(%err, "could not read the seat's keymap"),
        }
    }

    fn set_modifiers(&mut self, modifiers: Modifiers) {
        self.modifiers = modifiers;
        if let Some(state) = self.xkb.as_mut() {
            state.update_mask(
                modifiers.depressed,
                modifiers.latched,
                modifiers.locked,
                0,
                0,
                modifiers.group,
            );
        }
    }

    /// What the user pressed, in their own layout.
    fn keysym(&self, keycode: u32) -> Option<Keysym> {
        // Wayland carries the kernel's keycodes; an xkb keymap is written
        // eight higher, for the same historical reason the board's own keymap
        // starts at nine.
        let state = self.xkb.as_ref()?;
        Some(state.key_get_one_sym((keycode + 8).into()))
    }
}

/// The virtual keyboard, and the keycode it gave each stroke.
struct Typist {
    keyboard: ZwpVirtualKeyboardV1,
    codes: HashMap<Stroke, u32>,
    /// Whether the last keymap handed over carried the spare key. Flipped on
    /// every handover — see [`Self::rearm`].
    spare: bool,
}

impl Typist {
    fn new(keyboard: ZwpVirtualKeyboardV1) -> std::io::Result<Self> {
        let codes = alphabet()
            .into_iter()
            .enumerate()
            // The protocol's keycodes are the kernel's, which are the keymap's
            // less the eight every xkb keymap is offset by.
            .map(|(index, stroke)| (stroke, FIRST_KEYCODE + index as u32 - 8))
            .collect();
        let mut typist = Self {
            keyboard,
            codes,
            spare: false,
        };
        typist.rearm()?;
        Ok(typist)
    }

    /// Hand the compositor this board's keymap again, in a form it has not
    /// just seen.
    ///
    /// A compositor forwards a virtual keyboard's keymap to the focused client
    /// only when it differs from the one already in force. That is the right
    /// economy for a keymap that never changes, and wrong here, because the
    /// client can change underneath it: an application that binds its keyboard
    /// *after* the last handover is given the session's own keymap, and the
    /// compositor's idea of what is in force does not move. The next letter
    /// then arrives as a keycode read against the wrong map — `a` comes out as
    /// `n`, and Backspace as the keypad's Enter, which in a dialog is the OK
    /// button.
    ///
    /// So each handover carries one spare key that the last one did not. It
    /// types nothing and is never pressed; it exists to make this a keymap the
    /// compositor has not got, so that it passes it on. Alternating is enough:
    /// the only map that can be in force is the session's own or the other
    /// half of this pair.
    ///
    /// Called when the board appears and when the cursor moves to a new text
    /// field — both the moments where the client on the other end may have
    /// changed, and both slow enough for the keymap compile it costs.
    fn rearm(&mut self) -> std::io::Result<()> {
        self.spare = !self.spare;
        let (fd, size) = keymap_file(&keymap(&alphabet(), self.spare))?;
        self.keyboard
            .keymap(KeymapFormat::XkbV1 as u32, fd.as_fd(), size);
        Ok(())
    }

    /// Hold the same modifiers the user is holding, so that a key passed
    /// through from their own keyboard arrives as the combination they pressed
    /// rather than as the bare letter.
    fn send_modifiers(&self, modifiers: Modifiers) {
        self.keyboard.modifiers(
            modifiers.depressed,
            modifiers.latched,
            modifiers.locked,
            modifiers.group,
        );
    }

    /// Tap a key: press and release, since nothing on this board is held.
    fn type_stroke(&self, stroke: Stroke, at: u32) -> bool {
        let Some(code) = self.codes.get(&stroke) else {
            return false;
        };
        self.keyboard.key(at, *code, 1);
        self.keyboard.key(at.wrapping_add(1), *code, 0);
        true
    }
}

/// The keyboard as the shell holds it.
#[derive(Default)]
pub struct Osk {
    pub board: Board,
    open: bool,
    /// Whether the application in front says a text field has the cursor.
    focused: bool,
    /// Whether the keyboard has already come up by itself for the field that
    /// has the cursor now. Without it, closing the keyboard over a still
    /// focused field would only re-open it on the next frame.
    offered: bool,
    /// A shell login offers the board once across redraws and successive fields.
    /// Kept separate from application text-input focus and from manual dismissal.
    shell_field_offered: bool,
    /// Whether the user has shown they have a keyboard of their own. It stops
    /// the board offering itself to any further text field; see
    /// [`Self::dismiss_for_typing`], which is what concludes it from a key
    /// pressed while the board was up, and [`Self::set_controller_in_hand`],
    /// which is the shell saying the same thing from what it can see beyond
    /// this board — including from a session before this one.
    keyboard_at_hand: bool,
    /// Pending `activate` / `deactivate`, applied on `done` as the input
    /// method protocol requires: the two are state, not notifications.
    pending_activate: bool,
    pending_deactivate: bool,
    /// Whether a virtual keyboard was handed over at startup, and so whether
    /// anything the board draws could reach an application.
    ///
    /// Held as an answer rather than asked of `typist` each time so the whole
    /// of the model above — when the keyboard appears, when it goes away, when
    /// the hint stands in for it — can be exercised without a compositor on
    /// the other end of a socket.
    ready: bool,
    method: Option<ZwpInputMethodV2>,
    typist: Option<Typist>,
    /// The physical keyboard, held only while the board is on screen.
    grab: Option<Grab>,
    /// For asking for that grab, which can only be done once the board is up.
    qh: Option<QueueHandle<Shell>>,
    /// How far the board is out of the display's bottom edge: 0 away, 1 up.
    ///
    /// A position rather than the moment it opened, because it has to outlive
    /// the board. Dismissing it hands the keys straight back — the application
    /// must not spend a quarter of a second unable to type — while the slab is
    /// still on its way down, so the drawing needs an answer for a board that,
    /// as far as everything else is concerned, has already gone. It is also
    /// what lets the two directions be one movement: a board dismissed halfway
    /// through arriving falls from where it is rather than snapping up first.
    slide: f32,
    /// Whether the board is typing into the shell itself rather than into
    /// whatever is in front. See [`Self::open_here`].
    here: bool,
}

/// How long the board takes to cross the display's bottom edge, either way.
///
/// The same going down as coming up. A keyboard that left faster than it
/// arrived would read as having been dropped rather than put away, and the
/// journey is the same journey.
const SLIDE: f32 = 0.24;

impl Osk {
    /// Bind the input method and the virtual keyboard for a seat.
    ///
    /// Neither is fatal by its absence. Without the virtual keyboard there is
    /// nothing to type with and the keyboard is not offered at all; without
    /// the input method it can still be summoned, but nothing will summon it.
    pub fn attach(
        &mut self,
        globals: &GlobalList,
        qh: &QueueHandle<Shell>,
        seat: &WlSeat,
    ) -> &mut Self {
        self.qh = Some(qh.clone());
        match globals.bind::<ZwpVirtualKeyboardManagerV1, _, _>(qh, 1..=1, ()) {
            Ok(manager) => {
                let keyboard = manager.create_virtual_keyboard(seat, qh, ());
                match Typist::new(keyboard) {
                    Ok(typist) => {
                        self.typist = Some(typist);
                        self.ready = true;
                    }
                    Err(err) => tracing::warn!(
                        %err,
                        "could not hand the compositor a keymap; the on-screen keyboard cannot type"
                    ),
                }
            }
            Err(BindError::NotPresent) => tracing::info!(
                "zwp_virtual_keyboard_manager_v1 unavailable; the on-screen keyboard is off"
            ),
            Err(err) => tracing::warn!(%err, "could not bind the virtual keyboard"),
        }

        match globals.bind::<ZwpInputMethodManagerV2, _, _>(qh, 1..=1, ()) {
            Ok(manager) => self.method = Some(manager.get_input_method(seat, qh, ())),
            Err(BindError::NotPresent) => tracing::info!(
                "zwp_input_method_manager_v2 unavailable; the keyboard will not come up by itself"
            ),
            Err(err) => tracing::warn!(%err, "could not bind the input method"),
        }
        self
    }

    /// Whether the shell can type at all. Everything else is gated on it: a
    /// keyboard that cannot put a character anywhere should not appear.
    pub fn can_type(&self) -> bool {
        self.ready
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether a text field has the cursor, as far as the application in front
    /// is prepared to say.
    pub fn field_focused(&self) -> bool {
        self.focused
    }

    /// Whether to tell the user how to summon the keyboard: a field is waiting
    /// for typing and the keyboard is not up. The caller adds the rest of the
    /// question — the guide menu is a different screen, and the hint belongs
    /// under an application, not under a menu.
    pub fn wants_hint(&self) -> bool {
        self.can_type() && self.focused && !self.open
    }

    /// Whether any of the board is still on the display.
    ///
    /// Wider than [`Self::is_open`], and the difference is the quarter second
    /// after it is dismissed: the keys have gone back to the application, and
    /// the slab has not gone anywhere yet. This is the question the drawing
    /// asks — and, with it, everything that has to hold still for the drawing:
    /// the surface stays raised above the application, and the bar stays out
    /// from under a board that is still coming down over it.
    pub fn is_on_screen(&self) -> bool {
        self.open || self.slide > 0.0
    }

    /// Advance the slide by `dt` and say where the board is now: 0 below the
    /// display's edge, 1 in place.
    ///
    /// One number in both directions, so that reversing mid-flight continues
    /// from where the board is instead of from where a restarted curve would
    /// put it — which is what a user changing their mind about a keyboard they
    /// have just summoned would otherwise see.
    pub fn animate(&mut self, dt: f32) -> f32 {
        let target = if self.open { 1.0 } else { 0.0 };
        let step = dt / SLIDE;
        self.slide = if self.slide < target {
            (self.slide + step).min(target)
        } else {
            (self.slide - step).max(target)
        };
        self.slide
    }

    /// Show it, from the middle of the board. Returns whether anything
    /// changed.
    pub fn open(&mut self) -> bool {
        if !self.can_type() {
            return false;
        }
        self.raise(false)
    }

    /// Show it for a field of the shell's own — the password a removal has to
    /// be authorised with, or the one an authorisation this session has been
    /// asked to prove needs.
    ///
    /// Two things differ, and both follow from there being no application
    /// involved. It opens whether or not a virtual keyboard was ever handed
    /// over, because nothing is going to be sent through one; and while it is
    /// up, [`Self::press`] types nothing anywhere, returning the keystroke for
    /// the shell to put where it belongs. That second part is not a
    /// convenience — a password sent through the virtual keyboard would go to
    /// whichever client happens to hold the keys.
    pub fn open_here(&mut self) -> bool {
        self.raise(true)
    }

    /// Synchronize an automatically offered shell input panel with the board.
    /// Redrawing a field must not undo a physical keystroke or a manual dismissal.
    /// Explicit keyboard shortcuts still use `open_here` to request it again.
    pub fn offer_shell_field(&mut self, typing: bool) -> bool {
        if !typing {
            let offered = std::mem::take(&mut self.shell_field_offered);
            return offered && self.types_here() && self.close();
        }
        if std::mem::replace(&mut self.shell_field_offered, true) || self.keyboard_at_hand {
            return false;
        }
        self.open_here()
    }

    /// Whether the board is holding the physical keyboard.
    ///
    /// Asked by the shell's own key repeat, which cannot see a release while
    /// this is true: the grab takes every key event, and it drops the releases
    /// — see [`Self::key`]. A key held down when the board comes up is one the
    /// shell will never be told the end of.
    pub fn has_the_keyboard(&self) -> bool {
        self.grab.is_some()
    }

    /// Whether what the board types belongs to the shell rather than to
    /// whatever is in front.
    pub fn types_here(&self) -> bool {
        self.here
    }

    fn raise(&mut self, here: bool) -> bool {
        if self.open && self.here == here {
            return false;
        }
        self.open = true;
        self.here = here;
        // Whatever was concluded from the last keystroke is off: somebody has
        // asked for this board, and the only two things that ask are the
        // controller and a text field the board is no longer refusing.
        self.keyboard_at_hand = false;
        // Only a board opened over a field counts as that field's offer. A
        // board summoned by hand where nothing has the cursor — over the bar,
        // or over an application that never announced anything — must not
        // spend the automatic opening that the *next* text field is owed:
        // dismissing a keyboard on the home screen would otherwise stop the
        // browser's search box from bringing one up ever again.
        self.offered = self.focused;
        // Every latch cleared with it: a Ctrl left armed from the last time
        // the board was up would silently make the first key of this one a
        // shortcut.
        self.board = Board {
            row: HOME.0,
            column: HOME.1,
            ..Board::default()
        };
        // Nothing is done to the slide: it rises from wherever it is, which is
        // the bottom edge in the ordinary case and part of the way down in the
        // one that matters — a board asked for again while the last one is
        // still leaving comes back up from there.
        self.rearm();
        self.hold_keyboard();
        true
    }

    /// Borrow the physical keyboard for as long as the board is up, so that
    /// the arrows drive it and Enter presses the key under the cursor.
    ///
    /// See [`Grab`] for why this is not simply a matter of taking keyboard
    /// focus, which is the one thing the board must never do.
    fn hold_keyboard(&mut self) {
        if self.grab.is_some() {
            return;
        }
        let (Some(method), Some(qh)) = (self.method.as_ref(), self.qh.as_ref()) else {
            // No input method: the board is still perfectly usable from the
            // controller, which is what a console has.
            return;
        };
        self.grab = Some(Grab::new(method.grab_keyboard(qh, ())));
        tracing::debug!("the on-screen keyboard has the physical keyboard");
    }

    /// Give it back. Every key goes straight to the application again.
    fn release_keyboard(&mut self) {
        let Some(grab) = self.grab.take() else {
            return;
        };
        // Nothing of the user's keyboard may outlive the board: a modifier
        // left set here is one the application carries into its next keystroke.
        if let Some(typist) = self.typist.as_ref() {
            typist.send_modifiers(Modifiers::default());
        }
        grab.keyboard.release();
        tracing::debug!("the physical keyboard goes back to the application");
    }

    /// Make sure the compositor is holding this board's keymap for whatever
    /// client is in front now. See [`Typist::rearm`] for why that has to be
    /// said again rather than assumed.
    fn rearm(&mut self) {
        if let Some(typist) = self.typist.as_mut() {
            if let Err(err) = typist.rearm() {
                tracing::warn!(%err, "could not hand the compositor the keymap again");
            }
        }
    }

    pub fn close(&mut self) -> bool {
        let was = self.open;
        self.open = false;
        self.here = false;
        self.release_keyboard();
        was
    }

    /// Put it away without the journey down.
    ///
    /// For the one case that is not the board being dismissed: the guide
    /// taking the display from it. The menu is a different screen rather than
    /// this one with the keyboard gone off it, and it is drawn over everything
    /// the board would be sliding through — so there is nothing for the fall
    /// to be seen against, and a menu opened and shut again inside a quarter
    /// of a second would otherwise hand back a keyboard caught halfway out of
    /// the bottom edge.
    pub fn dismiss_at_once(&mut self) -> bool {
        self.slide = 0.0;
        self.close()
    }

    /// Apply what the input method said. Returns whether the keyboard should
    /// now be redrawn.
    fn set_focused(&mut self, focused: bool) -> bool {
        if self.focused == focused {
            return false;
        }
        self.focused = focused;
        if focused {
            // The field itself asks for the keyboard, once — unless the user
            // has already answered that question with a keyboard of their own.
            // Closing it leaves the hint in its place until the cursor moves
            // on.
            if !self.offered && !self.keyboard_at_hand {
                self.open();
            } else if self.open {
                // Already up, and the cursor has moved to a field that may
                // belong to a different client than the last letter went to.
                self.rearm();
            }
        } else {
            // The field is gone, so the keyboard has nowhere to type and the
            // hint has nothing to point at. The next field asks again.
            self.offered = false;
            self.close();
        }
        true
    }

    /// Act on a key from the physical keyboard, and say what the shell has to
    /// do about it.
    ///
    /// Presses only. A release is the tail of a press the board has already
    /// left the screen over, or of one made before it ever appeared; either
    /// way there is nothing left to do about it, and the board holds nothing
    /// down of its own.
    pub fn key(&mut self, keycode: u32, pressed: bool) -> Typed {
        let Some(grab) = self.grab.as_ref() else {
            return Typed::Ignored;
        };
        if !pressed {
            return Typed::Ignored;
        }
        match grab.keysym(keycode) {
            Some(keysym) => interpret(keysym),
            // A key the session's keymap has no symbol for. It is still a key
            // the user pressed, and the board is still in their way.
            None => Typed::Close,
        }
    }

    /// Put the board away because the user typed on a keyboard of their own,
    /// and remember that they have one.
    ///
    /// The remembering is the point. Closing alone would leave the board free
    /// to reappear over the very next field the application announces, and it
    /// would keep doing so all evening: the user has said once that they can
    /// type, and being told again at every text box is the behaviour they were
    /// trying to dismiss. The shortcut still summons it — see [`Self::open`],
    /// which is what forgets this — so a user who puts the keyboard down and
    /// picks the controller back up is one button from having it again.
    pub fn dismiss_for_typing(&mut self) -> bool {
        self.keyboard_at_hand = true;
        self.close()
    }

    /// The same conclusion, reached from outside and in both directions: which
    /// control the shell has last seen in the user's hands.
    ///
    /// [`Self::dismiss_for_typing`] can only know about a key pressed while the
    /// board was up, holding the grab that carried it. The shell knows more —
    /// keys pressed on its own screens, keys the compositor says went to an
    /// application, a thumb landing anywhere on a pad — and it remembers the
    /// answer across sessions, because which control somebody reaches for is a
    /// fact about them rather than about this session. A user who spent last
    /// night typing must not have a keyboard thrown over the first text field
    /// of the morning.
    ///
    /// Only what the board would offer *by itself* is affected. Asking for it —
    /// from the controller, from the compositor's binding, from the shell's own
    /// password field — raises it whichever hand asked.
    pub fn set_controller_in_hand(&mut self, in_hand: bool) {
        self.keyboard_at_hand = !in_hand;
    }

    /// Type something the board itself was not used for: a key the user
    /// pressed on their own keyboard, passed on to the application as though
    /// the matching key on the board had been.
    pub fn send(&self, stroke: Stroke, at: u32) -> bool {
        let modifiers = self
            .grab
            .as_ref()
            .map(|grab| grab.modifiers.forwarded().depressed)
            .unwrap_or_default();
        self.type_stroke(stroke, modifiers, at)
    }

    /// Send one stroke with a modifier mask held around it.
    ///
    /// Held either side of the key rather than left standing, so that nothing
    /// pressed afterwards is silently a chord — which matters here more than
    /// on a real keyboard, because the board's modifiers latch and the
    /// application has no way to see them let go.
    fn type_stroke(&self, stroke: Stroke, modifiers: u32, at: u32) -> bool {
        let Some(typist) = self.typist.as_ref() else {
            return false;
        };
        let held = Modifiers {
            depressed: modifiers,
            ..Default::default()
        };
        if !held.is_empty() {
            typist.send_modifiers(held);
        }
        let sent = typist.type_stroke(stroke, at);
        if !held.is_empty() {
            typist.send_modifiers(Modifiers::default());
        }
        sent
    }

    /// Press the selected key, sending whatever it types. `at` is a
    /// millisecond timestamp, which the protocol requires to keep increasing.
    pub fn press(&mut self, at: u32) -> Press {
        // Read before the press, which spends whatever was armed for it.
        let modifiers = self.board.modifiers();
        let press = self.board.press();
        // A board typing into the shell sends nothing anywhere: the caller
        // takes the stroke out of the returned `Press` and puts it where it
        // belongs. See `open_here`.
        if self.here {
            if press == Press::Close {
                self.close();
            }
            return press;
        }
        if let Press::Type(stroke) = press {
            // The board can only select keys the keymap was built from, so a
            // failure here is a keymap the compositor rejected rather than a
            // gap in the alphabet.
            if !self.type_stroke(stroke, modifiers, at) {
                tracing::warn!(?stroke, "nothing typed: no virtual keyboard");
            }
        }
        if press == Press::Close {
            self.close();
        }
        press
    }

    /// Finish the typing: Enter, and then the board away. What the Start
    /// button does while the board is up — see [`crate::model::Action::Submit`].
    ///
    /// The two keys it stands for are at opposite ends of the board — Enter on
    /// the right of the home row, the way out down in the corner — and a field
    /// that has been filled in is nearly always finished with both. Sent as
    /// though Enter's own key had been pressed, latches and all, so that a
    /// board holding Ctrl means the same thing whichever button sends the
    /// keystroke.
    ///
    /// Returns what was typed, on the same terms as [`Self::press`]: a board
    /// typing into the shell sends nothing anywhere and hands the stroke back
    /// for the caller to put where it belongs. It is closed either way before
    /// this returns, which is what lets the caller act on that stroke freely —
    /// a network address accepted here answers with a password field and a
    /// board of its own, and that board must not be the one this press was
    /// putting away.
    pub fn submit(&mut self, at: u32) -> Press {
        // Read before the press, as in `press`: it spends what was armed.
        let modifiers = self.board.modifiers();
        let press = self.board.submit();
        if let (false, Press::Type(stroke)) = (self.here, press) {
            if !self.type_stroke(stroke, modifiers, at) {
                tracing::warn!(?stroke, "nothing typed: no virtual keyboard");
            }
        }
        self.close();
        press
    }
}

// The manager globals have no events; the objects are kept only so the
// requests made on them stay alive.
impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for Shell {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardManagerV1,
        _event: <ZwpVirtualKeyboardManagerV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for Shell {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardV1,
        _event: <ZwpVirtualKeyboardV1 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodManagerV2, ()> for Shell {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpInputMethodManagerV2,
        _event: <ZwpInputMethodManagerV2 as wayland_client::Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for Shell {
    fn event(
        state: &mut Self,
        _proxy: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_keyboard_grab_v2::Event::Keymap { format, fd, size } => {
                if let (Some(grab), Ok(format)) = (state.osk.grab.as_mut(), format.into_result()) {
                    grab.load_keymap(format, fd, size);
                }
            }
            zwp_input_method_keyboard_grab_v2::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                if let Some(grab) = state.osk.grab.as_mut() {
                    grab.set_modifiers(Modifiers {
                        depressed: mods_depressed,
                        latched: mods_latched,
                        locked: mods_locked,
                        group,
                    });
                }
            }
            zwp_input_method_keyboard_grab_v2::Event::Key {
                key,
                state: key_state,
                ..
            } => {
                let pressed = matches!(key_state.into_result(), Ok(KeyState::Pressed));
                let typed = state.osk.key(key, pressed);
                state.on_typed(typed);
            }
            // `repeat_info` arrives with every grab and is ignored: the board
            // is gone by the second event any held key could produce, and the
            // repeat after that belongs to the application the key reaches.
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for Shell {
    fn event(
        state: &mut Self,
        _proxy: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            // Activation is double-buffered state, not a notification: it
            // takes effect on the `done` that follows, together with whatever
            // else the same batch said.
            zwp_input_method_v2::Event::Activate => state.osk.pending_activate = true,
            zwp_input_method_v2::Event::Deactivate => state.osk.pending_deactivate = true,
            zwp_input_method_v2::Event::Done => {
                let mut wanted = state.osk.field_focused();
                if state.osk.pending_activate {
                    wanted = true;
                }
                if state.osk.pending_deactivate {
                    wanted = false;
                }
                state.osk.pending_activate = false;
                state.osk.pending_deactivate = false;
                if state.osk.set_focused(wanted) {
                    tracing::debug!(focused = wanted, "a text field changed hands");
                    state.needs_redraw = true;
                }
            }
            // Somebody else is the input method now. The keyboard can still be
            // summoned and can still type; it just stops noticing text fields.
            zwp_input_method_v2::Event::Unavailable => {
                tracing::info!("another input method took the seat; text fields go unnoticed");
                // The board and its keyboard grab go first. The grab is a
                // child of the input method, so releasing it after destroying
                // its parent would be a request on an object the compositor
                // has already taken down with it.
                if state.osk.set_focused(false) {
                    state.needs_redraw = true;
                }
                state.osk.close();
                if let Some(method) = state.osk.method.take() {
                    method.destroy();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test at a time, because the caps are a fact about the *session*.
    ///
    /// [`CAPS`] is a static — reading a keymap is a file opened and a grammar
    /// parsed, and a board built fresh for every text field could not afford to
    /// do it — so a test that sets a layout changes what every other test's
    /// board is made of. Nearly every test in here presses a key or asks what
    /// one says, so nearly every one of them takes this.
    static LOCK: Mutex<()> = Mutex::new(());

    /// Hold the lock, and put the session's arrangement back when the test
    /// ends however it ends. A panicking test must not leave a Polish keyboard
    /// behind for whatever runs next.
    struct Held(
        #[allow(dead_code)] std::sync::MutexGuard<'static, ()>,
        Option<Arrangement>,
    );

    impl Drop for Held {
        fn drop(&mut self) {
            *CAPS.lock().unwrap() = self.1.take();
        }
    }

    fn alone() -> Held {
        let held = LOCK.lock().unwrap_or_else(|held| held.into_inner());
        let caps = CAPS.lock().unwrap().clone();
        Held(held, caps)
    }

    fn key_at(board: &Board) -> Key {
        let (row, column) = board.selected();
        row_keys(row)[column]
    }

    /// A board with the cursor on one key.
    fn at(row: usize, column: usize) -> Board {
        Board {
            row,
            column,
            ..Board::default()
        }
    }

    /// The left-hand Shift key's place: first of the row `z` is on.
    const SHIFT: (usize, usize) = (4, 0);
    /// `q`, first letter of the row Tab opens.
    const Q: (usize, usize) = (2, 1);

    /// The cursor the pointer moves is the same cursor the D-pad moves, so
    /// that the one button which presses a key presses the key being looked
    /// at however the user got there.
    #[test]
    fn a_pointer_puts_the_cursor_on_the_key_it_is_over() {
        let _held = alone();
        let mut board = at(HOME.0, HOME.1);
        assert!(board.select(Q.0, Q.1));
        assert_eq!(board.selected(), Q);
        assert_eq!(key_at(&board), Key::letter('q', 'Q'));
        // Pressing it types what is under the cursor, whoever put it there.
        assert_eq!(board.press(), Press::Type(Stroke::Char('q')));

        // Already there is not a move, which is what keeps a mouse resting on
        // one key from asking for a redraw on every motion event.
        assert!(!board.select(Q.0, Q.1));

        // A hit test that found nothing is refused rather than clamped to the
        // nearest key: the nearest key is not what was pointed at.
        assert!(!board.select(ROW_COUNT, 0));
        assert!(!board.select(Q.0, row_keys(Q.0).len()));
        assert_eq!(board.selected(), Q);
    }

    #[test]
    fn every_row_fits_the_grid_it_is_centred_in() {
        let _held = alone();
        for row in 0..ROW_COUNT {
            let layout = row_layout(row);
            assert!(!layout.is_empty(), "row {row} has no keys");
            let (start, span) = *layout.last().unwrap();
            assert!(
                start + span <= COLUMNS + f32::EPSILON,
                "row {row} runs {} columns past the grid",
                start + span - COLUMNS
            );
            assert!(
                layout[0].0 >= -f32::EPSILON,
                "row {row} starts off the left"
            );
            // Centred, so the margins match.
            let left = layout[0].0;
            let right = COLUMNS - (start + span);
            assert!((left - right).abs() < 1e-4, "row {row} is not centred");
        }
        // ANSI's own proportions: every row is exactly the width of the grid,
        // which is what makes Tab, Caps, Enter and the two Shifts line up down
        // the sides — and what reaches the function row to both edges rather
        // than leaving it inset with a gap at each end.
        for row in 0..ROW_COUNT {
            let width: f32 = row_spans(row).iter().map(|(_, span)| *span).sum();
            assert!(
                (width - COLUMNS).abs() < 1e-4,
                "row {row} is {width} columns, not {COLUMNS}"
            );
            assert!(row_layout(row)[0].0.abs() < 1e-4, "row {row} is inset");
        }
    }

    /// The function row is the one rank drawn short. Everything else is a full
    /// keycap tall, and the board is built on that being true of exactly one
    /// row — see `ui::row_band`.
    #[test]
    fn only_the_function_row_is_drawn_short() {
        let _held = alone();
        assert!(row_scale(FUNCTION_ROW) < 1.0);
        for row in 0..ROW_COUNT {
            let scale = row_scale(row);
            assert!(scale > 0.0 && scale <= 1.0, "row {row} is {scale} tall");
            if row != FUNCTION_ROW {
                assert_eq!(scale, 1.0, "row {row} should be a full key tall");
            }
        }
    }

    /// Whether this machine's xkeyboard-config can compile a layout, which is
    /// what every test below needs and what a build container may not have.
    /// See [[tests-that-read-the-machine]].
    fn has(layout: &str, variant: &str) -> bool {
        read_arrangement(layout, variant).is_some()
    }

    /// What one row of the board says, key by key, on its plain face.
    fn printed(row: usize) -> Vec<String> {
        row_keys(row)
            .into_iter()
            .map(|key| key.cap(Level::Plain))
            .collect()
    }

    /// The caps come off the layout, and the positions do not move.
    ///
    /// German is the clearest pair to check both halves at once: it is QWERTZ,
    /// so the key ANSI prints Y types z and the key it prints Z types y — the
    /// letters swapped and the *keys* exactly where they were.
    #[test]
    fn the_caps_come_off_the_layout_and_the_keys_stay_where_they_are() {
        let _held = alone();
        if !has("de", "") {
            return;
        }
        assert!(note_layout("de", ""));

        // The lower row is a Shift, ten characters and a Shift.
        assert_eq!(
            printed(4)[1..11],
            ["y", "x", "c", "v", "b", "n", "m", ",", ".", "-"]
        );
        // The upper row is Tab, twelve characters and the backslash's key.
        assert_eq!(printed(2)[1..7], ["q", "w", "e", "r", "t", "z"]);

        // And every row is still exactly the width of the grid. A layout may
        // say what the keys print; it may not say how many there are.
        for row in 0..ROW_COUNT {
            let width: f32 = row_spans(row).iter().map(|(_, span)| *span).sum();
            assert!(
                (width - COLUMNS).abs() < 1e-4,
                "row {row} is {width} columns, not {COLUMNS}"
            );
        }
    }

    /// A layout that keeps letters behind AltGr grows the key that reaches
    /// them, and pressing it changes what the caps say.
    ///
    /// Polish is the case this exists for: it is QWERTY, so without AltGr the
    /// board would look right and be unable to write a single Polish word.
    #[test]
    fn a_layout_with_letters_behind_altgr_grows_the_key_that_reaches_them() {
        let _held = alone();
        if !has("pl", "") {
            return;
        }
        assert!(note_layout("pl", ""));

        let bottom = row_keys(ROW_COUNT - 1);
        assert!(
            bottom.contains(&Key::AltGr),
            "a layout with a third level has the key for it: {bottom:?}"
        );

        // AltGr and the key ANSI prints A, which on this layout is ą.
        let mut board = at(HOME.0, HOME.1);
        assert_eq!(board.press(), Press::Type(Stroke::Char('a')));
        let altgr = bottom.iter().position(|key| *key == Key::AltGr).unwrap();
        let mut board = at(ROW_COUNT - 1, altgr);
        assert_eq!(board.press(), Press::Shifted);
        assert_eq!(board.level(), Level::AltGr);
        board.select(HOME.0, HOME.1);
        assert_eq!(key_at(&board).cap(Level::AltGr), "ą");
        assert_eq!(board.press(), Press::Type(Stroke::Char('ą')));
        // Spent by the key it was armed for, like every other latch.
        assert_eq!(board.level(), Level::Plain);
    }

    /// An arrangement with one key on AltGr and nothing else there.
    ///
    /// Built rather than read off a layout, because which keys a real layout
    /// leaves blank is xkeyboard-config's business and changes with the
    /// package: Polish, the obvious candidate, includes `latin` and so has
    /// something on AltGr for every key on the board.
    fn one_key_on_altgr() -> Arrangement {
        let mut rows: [Vec<Cap>; 4] = Default::default();
        for (index, keycodes) in KEYCODES.iter().enumerate() {
            rows[index] = keycodes.iter().map(|_| Cap::letter('a', 'A')).collect();
        }
        rows[2][0] = Cap {
            levels: [
                Some(Stroke::Char('a')),
                Some(Stroke::Char('A')),
                Some(Stroke::Char('ą')),
                None,
            ],
        };
        Arrangement { rows, altgr: true }
    }

    /// A key with nothing on the face the board is showing types nothing, and
    /// does not spend the modifier that was armed for the key next to it.
    #[test]
    fn a_blank_cap_types_nothing_and_keeps_the_modifier_it_was_armed_with() {
        let _held = alone();
        *CAPS.lock().unwrap() = Some(one_key_on_altgr());

        let bottom = row_keys(ROW_COUNT - 1);
        let altgr = bottom
            .iter()
            .position(|key| *key == Key::AltGr)
            .expect("the arrangement has a third level");
        let mut board = at(ROW_COUNT - 1, altgr);
        assert_eq!(board.press(), Press::Shifted);

        // The one key that has something there types it, and spends the latch.
        board.select(3, 1);
        assert_eq!(key_at(&board).cap(Level::AltGr), "ą");

        // Its neighbour has nothing there, and the press changes nothing at
        // all — the armed AltGr included, because the user is still reaching
        // for the key it was armed for.
        board.select(3, 2);
        assert_eq!(key_at(&board).cap(Level::AltGr), "");
        assert_eq!(board.press(), Press::Nothing);
        assert_eq!(
            board.level(),
            Level::AltGr,
            "a key that did nothing takes nothing away"
        );
        board.select(3, 1);
        assert_eq!(board.press(), Press::Type(Stroke::Char('ą')));
        assert_eq!(board.level(), Level::Plain);
    }

    /// A US board has no AltGr key, because on that layout it would do nothing.
    #[test]
    fn a_layout_with_nothing_on_the_far_faces_draws_no_altgr() {
        let _held = alone();
        if !has("us", "") {
            return;
        }
        assert!(note_layout("us", ""));
        assert!(!row_keys(ROW_COUNT - 1).contains(&Key::AltGr));
        assert_eq!(
            row_keys(ROW_COUNT - 1).len(),
            8,
            "Ctrl, Alt, Space, four arrows and the way out"
        );
    }

    /// A dead key shows the accent it carries and sends the keysym itself,
    /// which is what the key on the desk sends.
    ///
    /// French is the layout this exists for: `^` there is `dead_circumflex`,
    /// and it is how every circumflex in the language is written.
    #[test]
    fn a_dead_key_shows_its_accent_and_sends_the_keysym() {
        let _held = alone();
        if !has("fr", "") {
            return;
        }
        assert!(note_layout("fr", ""));

        // <AD11>, which ANSI prints [ and AZERTY prints the circumflex.
        let dead = row_keys(2)[11];
        assert_eq!(dead.cap(Level::Plain), "^");
        let Key::Char(cap) = dead else {
            panic!("a character key: {dead:?}");
        };
        let Some(Stroke::Keysym(raw)) = cap.at(Level::Plain) else {
            panic!("a dead key sends a keysym: {cap:?}");
        };
        assert_eq!(xkb::keysym_get_name(Keysym::new(raw)), "dead_circumflex");
        // And it has a key of its own in the keymap the board uploads, written
        // under the name xkbcommon will parse back.
        assert!(keymap(&alphabet(), false).contains("[ dead_circumflex ]"));
    }

    /// A layout that will not compile leaves the board exactly as it was.
    ///
    /// The one failure a keyboard may not have: a board that fell back to
    /// something else would be a board somebody cannot type their password on,
    /// with nothing on screen saying why.
    #[test]
    fn a_layout_that_will_not_compile_leaves_the_board_alone() {
        let _held = alone();
        if !has("us", "") {
            return;
        }
        assert!(note_layout("us", ""));
        // Back to the board's own ANSI arrangement, not on to a guess and not
        // left on the layout before it: the shell cannot know what the keys say
        // now, and the one thing it must not do is print letters that are not
        // there.
        assert!(note_layout("no-such-layout-anywhere", ""));
        assert!(CAPS.lock().unwrap().is_none());
        assert_eq!(
            printed(3)[1..12],
            HOME_ROW.0.chars().map(String::from).collect::<Vec<_>>()[..]
        );
    }

    /// With nothing said, the board is the ANSI/US arrangement it has always
    /// been — which is what a session on a compositor too old to say gets.
    #[test]
    fn a_board_nothing_has_told_a_layout_keeps_the_ansi_one() {
        let _held = alone();
        *CAPS.lock().unwrap() = None;
        // Caps, the eleven characters of the home row, and Enter.
        assert_eq!(printed(3)[1..12].concat(), HOME_ROW.0);
        assert_eq!(printed(1).concat(), format!("{}Back", NUMBER_ROW.0));
        assert!(!row_keys(ROW_COUNT - 1).contains(&Key::AltGr));
    }

    #[test]
    fn the_arrangement_is_the_one_printed_on_an_ansi_keyboard() {
        let _held = alone();
        let caps = |row: usize| -> Vec<String> {
            row_keys(row)
                .into_iter()
                .map(|key| key.cap(Level::Plain))
                .collect()
        };
        // Esc and twelve function keys, the backtick opening the number row,
        // the backslash closing the row `p` is on — the three placements a
        // non-ANSI board gets wrong.
        assert_eq!(caps(0).first().map(String::as_str), Some("Esc"));
        assert_eq!(caps(0).last().map(String::as_str), Some("F12"));
        assert_eq!(caps(0).len(), 13);
        assert_eq!(caps(1).first().map(String::as_str), Some("`"));
        assert_eq!(caps(2).last().map(String::as_str), Some("\\"));

        // Tab, Caps and Shift open their rows; Back, Enter and the second
        // Shift close them.
        assert_eq!(row_keys(2)[0], Key::Named("Tab", Stroke::TAB));
        assert_eq!(row_keys(3)[0], Key::Caps);
        assert_eq!(row_keys(4)[0], Key::Shift);
        assert_eq!(row_keys(4).last().copied(), Some(Key::Shift));
        assert_eq!(caps(1).last().map(String::as_str), Some("Back"));
        assert_eq!(caps(3).last().map(String::as_str), Some("Enter"));

        // And the bottom row: the two modifiers where ANSI puts them, the
        // space bar, the arrows in reading order, and the way out at the far
        // end.
        let bottom = row_keys(ROW_COUNT - 1);
        assert_eq!(
            bottom,
            vec![
                Key::Ctrl,
                Key::Alt,
                Key::Named("Space", Stroke::SPACE),
                Key::Arrow(Arrow::Left),
                Key::Arrow(Arrow::Down),
                Key::Arrow(Arrow::Up),
                Key::Arrow(Arrow::Right),
                Key::Close,
            ]
        );

        // The cursor opens on `a`, wherever the rows have moved to.
        assert_eq!(row_keys(HOME.0)[HOME.1], Key::letter('a', 'A'));
    }

    #[test]
    fn shift_changes_the_caps_and_what_is_typed() {
        let _held = alone();
        let mut board = at(Q.0, Q.1);
        assert_eq!(key_at(&board).cap(board.level()), "q");
        assert_eq!(board.press(), Press::Type(Stroke::Char('q')));

        (board.row, board.column) = SHIFT;
        assert_eq!(board.press(), Press::Shifted);
        assert!(board.shifted() && !board.locked(Key::Shift));

        (board.row, board.column) = Q;
        assert_eq!(key_at(&board).cap(board.level()), "Q");
        assert_eq!(board.press(), Press::Type(Stroke::Char('Q')));
        // One-shot: the capital used it up.
        assert!(!board.shifted());
        assert_eq!(board.press(), Press::Type(Stroke::Char('q')));
    }

    /// Caps Lock is the lock on its own: the reason to have it as well as
    /// Shift is that a run of capitals should not have to pass through
    /// armed-for-one on the way.
    #[test]
    fn caps_lock_goes_straight_to_the_lock_and_straight_back_off_it() {
        let _held = alone();
        let caps = (3, 0);
        let mut board = at(caps.0, caps.1);
        assert_eq!(key_at(&board), Key::Caps);
        assert_eq!(board.press(), Press::Shifted);
        assert!(board.locked(Key::Shift), "one press should lock it");

        (board.row, board.column) = Q;
        assert_eq!(board.press(), Press::Type(Stroke::Char('Q')));
        assert_eq!(board.press(), Press::Type(Stroke::Char('Q')));
        assert!(board.locked(Key::Shift), "the lock outlived the capitals");

        (board.row, board.column) = caps;
        board.press();
        assert!(!board.shifted());

        // And it takes a Shift that was armed for one letter straight to the
        // lock rather than cancelling it.
        (board.row, board.column) = SHIFT;
        board.press();
        assert!(board.shifted() && !board.locked(Key::Shift));
        (board.row, board.column) = caps;
        board.press();
        assert!(board.locked(Key::Shift));
    }

    /// A latched key stays lit while it is holding the board, and only a
    /// locked one says so with the brighter of the two tints.
    #[test]
    fn the_keys_that_changed_the_board_stay_lit() {
        let _held = alone();
        let mut board = at(SHIFT.0, SHIFT.1);
        assert_eq!(board.latched(Key::Shift), Latch::Off);
        board.press();
        assert_eq!(board.latched(Key::Shift), Latch::Once);
        assert_eq!(
            board.latched(Key::Caps),
            Latch::Off,
            "Caps names the lock, and a shift armed for one letter is not one"
        );
        board.press();
        assert_eq!(board.latched(Key::Shift), Latch::Locked);
        assert_eq!(board.latched(Key::Caps), Latch::Locked);

        // A letter is never lit for holding anything.
        assert_eq!(board.latched(Key::letter('a', 'A')), Latch::Off);
        assert_eq!(board.latched(Key::Close), Latch::Off);
    }

    /// Ctrl and Alt latch the way Shift does, because a board driven one key
    /// at a time has no way to hold anything down — there is only ever one
    /// finger on it.
    #[test]
    fn ctrl_and_alt_latch_and_are_sent_with_the_key_that_follows() {
        let _held = alone();
        const CTRL: (usize, usize) = (ROW_COUNT - 1, 0);
        const ALT: (usize, usize) = (ROW_COUNT - 1, 1);

        let mut board = at(CTRL.0, CTRL.1);
        assert_eq!(key_at(&board), Key::Ctrl);
        assert_eq!(board.modifiers(), 0);
        assert_eq!(board.press(), Press::Shifted, "nothing is typed by itself");
        assert_eq!(board.latched(Key::Ctrl), Latch::Once);
        assert_eq!(board.modifiers(), MOD_CONTROL);

        // The next key carries it, and spends it.
        (board.row, board.column) = (2, 3);
        assert_eq!(key_at(&board), Key::letter('e', 'E'));
        assert_eq!(board.press(), Press::Type(Stroke::Char('e')));
        assert_eq!(board.modifiers(), 0, "the armed Ctrl outlived its key");

        // Pressed twice it locks, and then it does not.
        (board.row, board.column) = CTRL;
        board.press();
        board.press();
        assert_eq!(board.latched(Key::Ctrl), Latch::Locked);
        (board.row, board.column) = (2, 3);
        board.press();
        assert_eq!(board.modifiers(), MOD_CONTROL, "a locked Ctrl was spent");

        // Alt is its own latch, and the two combine.
        (board.row, board.column) = ALT;
        assert_eq!(key_at(&board), Key::Alt);
        board.press();
        assert_eq!(board.modifiers(), MOD_CONTROL | MOD_ALT);

        // Shift is not in the mask: the board reaches a capital by sending the
        // key for `E`, not by sending `e` with a modifier, so nothing can be
        // left latched in the application if the board goes away mid-word.
        (board.row, board.column) = SHIFT;
        board.press();
        assert!(board.shifted());
        assert_eq!(board.modifiers() & 0x1, 0);
    }

    /// Start types Enter from wherever the cursor is standing, and is a
    /// keystroke like any other while it does: an armed modifier is spent on
    /// it, a locked one is not, and the cursor stays where it was.
    #[test]
    fn start_types_enter_without_the_cursor_being_on_it() {
        let _held = alone();
        const CTRL: (usize, usize) = (ROW_COUNT - 1, 0);

        let mut board = at(HOME.0, HOME.1);
        assert_eq!(key_at(&board), Key::letter('a', 'A'));
        assert_eq!(board.submit(), Press::Type(Stroke::ENTER));
        assert_eq!(
            board.selected(),
            HOME,
            "the board is going away; where it stood is where it comes back"
        );

        // A latch armed for the next key is armed for this one.
        (board.row, board.column) = CTRL;
        board.press();
        assert_eq!(board.modifiers(), MOD_CONTROL);
        (board.row, board.column) = HOME;
        assert_eq!(board.submit(), Press::Type(Stroke::ENTER));
        assert_eq!(board.modifiers(), 0, "the armed Ctrl outlived its key");
    }

    #[test]
    fn a_second_press_of_shift_locks_it() {
        let _held = alone();
        let mut board = at(SHIFT.0, SHIFT.1);
        board.press();
        board.press();
        assert!(board.locked(Key::Shift));

        (board.row, board.column) = Q;
        assert_eq!(board.press(), Press::Type(Stroke::Char('Q')));
        assert_eq!(board.press(), Press::Type(Stroke::Char('Q')));
        assert!(
            board.locked(Key::Shift),
            "a locked shift outlives the character"
        );

        (board.row, board.column) = SHIFT;
        board.press();
        assert!(!board.shifted());
    }

    #[test]
    fn up_and_down_land_on_the_key_across_rather_than_the_key_numbered_the_same() {
        let _held = alone();
        // `a` is the second key of its row, and the row below it opens with a
        // Shift a quarter of a column wider than the Caps Lock above it — so
        // stepping by index would put Down from `a` on `x`, one key to the
        // right of where it looks.
        let mut board = at(HOME.0, HOME.1);
        assert_eq!(key_at(&board), Key::letter('a', 'A'));
        board.move_selection(Move::Down);
        assert_eq!(key_at(&board), Key::letter('z', 'Z'));

        // And upwards out of a key five and a half columns wide: what the
        // middle of the space bar is under, not the third key of the row above
        // it.
        (board.row, board.column) = (ROW_COUNT - 1, 2);
        assert_eq!(key_at(&board), Key::Named("Space", Stroke::SPACE));
        board.move_selection(Move::Up);
        assert_eq!(key_at(&board), Key::letter('v', 'V'));

        // The arrows sit under the keys they are in line with, so a cursor
        // walking down the right-hand side of the board arrives on them rather
        // than skidding past onto Close.
        (board.row, board.column) = (4, 10);
        assert_eq!(key_at(&board), Key::letter('/', '?'));
        board.move_selection(Move::Down);
        assert_eq!(key_at(&board), Key::Arrow(Arrow::Right));
        board.move_selection(Move::Up);
        assert_eq!(key_at(&board), Key::letter('/', '?'));

        // A key sitting exactly between two of them goes to the left one.
        // Arbitrary, but fixed: ANSI's rows are a quarter and a half column
        // out of step with their neighbours, so ties are common rather than an
        // edge case, and they have to break the same way every time.
        (board.row, board.column) = (3, 10);
        assert_eq!(key_at(&board), Key::letter(';', ':'));
        board.move_selection(Move::Down);
        assert_eq!(key_at(&board), Key::letter('.', '>'));
    }

    #[test]
    fn moving_off_an_end_wraps_rather_than_sticking() {
        let _held = alone();
        let mut board = at(0, 0);
        assert_eq!(key_at(&board), Key::Named("Esc", Stroke::ESCAPE));
        board.move_selection(Move::Left);
        assert_eq!(
            key_at(&board),
            Key::Named("F12", Stroke::Named("F12")),
            "wrapped to the row's end"
        );

        // Up from the function row reaches the bottom one, not nothing.
        board.column = 0;
        board.move_selection(Move::Up);
        assert_eq!(board.selected().0, ROW_COUNT - 1);
        board.move_selection(Move::Down);
        assert_eq!(board.selected().0, 0);
    }

    #[test]
    fn the_keymap_gives_every_key_the_board_can_send_a_code_of_its_own() {
        let _held = alone();
        let alphabet = alphabet();
        let text = keymap(&alphabet, false);

        // Every character on the board, in both cases, plus every key that is
        // not a character at all: Esc, Tab, Back, Enter, Space, the function
        // row and the arrows.
        for row in 0..ROW_COUNT {
            for key in row_keys(row) {
                let wanted: Vec<Stroke> = match key {
                    Key::Char(cap) => cap.strokes().collect(),
                    Key::Named(_, stroke) => vec![stroke],
                    Key::Arrow(arrow) => vec![arrow.stroke()],
                    Key::Shift | Key::Caps | Key::Ctrl | Key::Alt | Key::AltGr | Key::Close => {
                        vec![]
                    }
                };
                for stroke in wanted {
                    assert!(
                        alphabet.contains(&stroke),
                        "{stroke:?} is on the board but not in the keymap"
                    );
                    assert!(
                        text.contains(&format!("[ {} ]", stroke.keysym())),
                        "{stroke:?} has no key in the keymap"
                    );
                }
            }
        }

        // Distinct codes, and all of them inside what evdev can carry.
        let mut codes: Vec<u32> = (0..alphabet.len())
            .map(|index| FIRST_KEYCODE + index as u32)
            .collect();
        let count = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), count);
        assert!(
            codes.last().is_some_and(|last| *last <= 255),
            "the keymap outgrew the range a keycode can be sent in"
        );
    }

    /// The bug this guards is silent and total: the compositor keeps the last
    /// keymap it was handed and only passes on a new one when it differs, so
    /// two identical handovers leave an application that bound its keyboard in
    /// between reading these keycodes against the session's own map. Every
    /// letter then comes out as a different letter.
    #[test]
    fn no_two_handovers_offer_the_compositor_the_same_keymap() {
        let _held = alone();
        let alphabet = alphabet();
        let plain = keymap(&alphabet, false);
        let spared = keymap(&alphabet, true);
        assert_ne!(plain, spared);

        // The spare key is the only difference, and it types nothing: every
        // stroke keeps the keycode it had, or the board would be sending one
        // letter's code for another's.
        assert!(spared.contains("VoidSymbol"));
        assert!(!plain.contains("VoidSymbol"));
        for (index, stroke) in alphabet.iter().enumerate() {
            let key = format!("  key <K{index}> {{ [ {} ] }};\n", stroke.keysym());
            assert!(plain.contains(&key), "{stroke:?} moved");
            assert!(spared.contains(&key), "{stroke:?} moved when spared");
        }
        // And the spare is past the end, so it has no code the board can send.
        let spare = FIRST_KEYCODE as usize + alphabet.len();
        assert!(spared.contains(&format!("<K{}> = {spare};", alphabet.len())));
        assert!(spared.contains(&format!("maximum = {};", spare + 1)));
    }

    #[test]
    fn characters_are_written_as_unicode_keysyms() {
        let _held = alone();
        assert_eq!(Stroke::Char('a').keysym(), "U0061");
        assert_eq!(Stroke::Char('~').keysym(), "U007E");
        assert_eq!(Stroke::Char(' ').keysym(), "U0020");
        // The keys with no character to spell that way carry the name xkb
        // already knows them by, spelt exactly as `keysymdef.h` has it.
        assert_eq!(Stroke::BACKSPACE.keysym(), "BackSpace");
        assert_eq!(Stroke::ENTER.keysym(), "Return");
        assert_eq!(Stroke::TAB.keysym(), "Tab");
        assert_eq!(Stroke::ESCAPE.keysym(), "Escape");
        assert_eq!(Arrow::Left.stroke().keysym(), "Left");
        assert_eq!(Arrow::Right.stroke().keysym(), "Right");
        for name in FUNCTION_KEYS {
            assert_eq!(Stroke::Named(name).keysym(), name);
        }
    }

    #[test]
    fn a_keyboard_with_nothing_to_type_with_never_opens() {
        let _held = alone();
        // No `attach`, so no virtual keyboard: the shell is running on a
        // compositor without the protocol.
        let mut osk = Osk::default();
        assert!(!osk.can_type());
        assert!(!osk.open(), "opened with nothing to type into");
        assert!(!osk.is_open());
        assert!(!osk.wants_hint(), "offered a shortcut that does nothing");
    }

    /// A keyboard the compositor has handed a virtual keyboard to. Nothing it
    /// types goes anywhere without one, so nothing else about it is exercised
    /// by a default [`Osk`].
    fn armed() -> Osk {
        Osk {
            ready: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_field_taking_the_cursor_brings_the_keyboard_up_once() {
        let _held = alone();
        let mut osk = armed();
        assert!(!osk.is_open());
        assert!(!osk.wants_hint(), "nothing is focused yet");

        // A search box takes the cursor: the keyboard appears without being
        // asked for. This is the whole of the automatic half of the feature.
        assert!(osk.set_focused(true));
        assert!(osk.is_open());
        assert!(!osk.wants_hint(), "the keyboard itself is the hint");

        // Put away by hand. The field still has the cursor, so the shortcut is
        // worth showing — and the keyboard must not simply come back.
        assert!(osk.close());
        assert!(!osk.is_open());
        assert!(osk.wants_hint());
        assert!(!osk.set_focused(true), "already focused; nothing changed");
        assert!(!osk.is_open(), "reopened a keyboard the user dismissed");

        // The cursor leaves the field: no keyboard, and nothing to hint at.
        assert!(osk.set_focused(false));
        assert!(!osk.is_open());
        assert!(!osk.wants_hint());

        // The next field is a fresh offer.
        assert!(osk.set_focused(true));
        assert!(osk.is_open());
    }

    #[test]
    fn the_keyboard_can_be_summoned_where_no_field_ever_announced_itself() {
        let _held = alone();
        // The case the shortcut exists for: an X11 client, or a browser
        // without Wayland IME, where `activate` will never arrive.
        let mut osk = armed();
        assert!(!osk.field_focused());
        assert!(osk.open());
        assert!(osk.is_open());
        assert!(osk.close());
        assert!(!osk.is_open());
        // Never focused, so there is no field for a hint to point at.
        assert!(!osk.wants_hint());

        // And summoning it by hand does not spend the offer a text field is
        // owed. This went wrong live: the board summoned and dismissed on the
        // home screen left the next application's search box with a corner
        // hint where it should have had a keyboard.
        assert!(osk.set_focused(true));
        assert!(
            osk.is_open(),
            "a hand-summoned board on the home screen swallowed the field's own"
        );
    }

    #[test]
    fn closing_by_the_boards_own_button_puts_it_away() {
        let _held = alone();
        let mut osk = armed();
        osk.open();
        // The Close key is the last of the function row.
        osk.board = at(ROW_COUNT - 1, row_keys(ROW_COUNT - 1).len() - 1);
        assert!(key_at(&osk.board).is_close());
        assert_eq!(osk.press(0), Press::Close);
        assert!(!osk.is_open());
    }

    /// What the user asked for, in the words they asked for it in: a key on a
    /// real keyboard is proof the board is not needed, whichever key it was.
    /// Nothing on it is navigable from a keyboard, because navigating a
    /// picture of the keys already under your hands is not a thing anybody
    /// wants to do.
    #[test]
    fn any_key_on_a_real_keyboard_puts_the_board_away() {
        let _held = alone();
        for keysym in [
            Keysym::Left,
            Keysym::Right,
            Keysym::Up,
            Keysym::Down,
            Keysym::KP_Down,
            Keysym::Return,
            Keysym::KP_Enter,
            Keysym::Escape,
            Keysym::q,
            Keysym::space,
            Keysym::F5,
            // Not a key the board has anything to say about, and still a key
            // the user pressed.
            Keysym::Print,
            Keysym::Shift_L,
        ] {
            assert!(
                !matches!(interpret(keysym), Typed::Ignored),
                "{keysym:?} left the board on screen"
            );
        }
    }

    /// And the other half, which matters just as much: the board is holding
    /// the physical keyboard when that key arrives, so nothing else is in a
    /// position to pass it on. Dismissing the board must not cost the letter
    /// that dismissed it.
    #[test]
    fn the_key_that_dismissed_the_board_is_typed_into_the_application() {
        let _held = alone();
        assert_eq!(interpret(Keysym::q), Typed::Send(Stroke::Char('q')));
        // The keysym arrives with the shift level already applied, which is
        // why the board needs no modifier to send a capital.
        assert_eq!(interpret(Keysym::Q), Typed::Send(Stroke::Char('Q')));
        assert_eq!(interpret(Keysym::space), Typed::Send(Stroke::Char(' ')));
        assert_eq!(interpret(Keysym::exclam), Typed::Send(Stroke::Char('!')));
        assert_eq!(interpret(Keysym::BackSpace), Typed::Send(Stroke::BACKSPACE));
        assert_eq!(interpret(Keysym::Tab), Typed::Send(Stroke::TAB));
        assert_eq!(
            interpret(Keysym::F5),
            Typed::Send(Stroke::Named("F5")),
            "the function row is on the board, so it can be typed"
        );
        assert_eq!(interpret(Keysym::F12), Typed::Send(Stroke::Named("F12")));

        // The keys with no character of their own are on the board too, and go
        // through for the same reason: a caret being arrowed through a field,
        // or a form finished with Return, must not lose the keystroke that
        // happened to be the first one.
        assert_eq!(interpret(Keysym::Return), Typed::Send(Stroke::ENTER));
        assert_eq!(interpret(Keysym::Left), Typed::Send(Arrow::Left.stroke()));
        assert_eq!(
            interpret(Keysym::KP_Down),
            Typed::Send(Arrow::Down.stroke())
        );

        // Everything the board can send is something the board can be sent.
        let alphabet = alphabet();
        for keysym in [
            Keysym::q,
            Keysym::Q,
            Keysym::BackSpace,
            Keysym::F1,
            Keysym::Return,
            Keysym::Up,
        ] {
            let Typed::Send(stroke) = interpret(keysym) else {
                panic!("{keysym:?} is not typed at all");
            };
            assert!(alphabet.contains(&stroke), "{stroke:?} has no key to send");
        }

        // Escape is the one key that dismisses without typing, which is what
        // it does on every keyboard that appears by itself — even though the
        // board has a cap for it and could send it.
        assert_eq!(interpret(Keysym::Escape), Typed::Close);
        assert!(alphabet.contains(&Stroke::ESCAPE));

        // A key with no character and no place on the board takes the board
        // away and goes nowhere itself, which is where it was already headed.
        assert_eq!(interpret(Keysym::Print), Typed::Close);
        assert_eq!(interpret(Keysym::Shift_L), Typed::Close);
    }

    /// Closing on a keystroke is only half of it. The board is offered to a
    /// text field once, and a user who has answered that offer by typing must
    /// not be asked again at the next field — that is the very thing they
    /// dismissed. The shortcut still brings it back, for the evening they put
    /// the keyboard down and pick the controller up.
    #[test]
    fn a_board_dismissed_by_typing_stops_offering_itself() {
        let _held = alone();
        let mut osk = armed();
        osk.set_focused(true);
        assert!(osk.is_open(), "the field's own offer");
        osk.dismiss_for_typing();
        assert!(!osk.is_open());

        // The cursor moves to another field, in this or another application.
        osk.set_focused(false);
        osk.set_focused(true);
        assert!(
            !osk.is_open(),
            "the board came back at the next field the user typed into"
        );

        // Until it is asked for by hand, which is the controller saying the
        // keyboard has been put down.
        assert!(osk.open());
        osk.close();
        osk.set_focused(false);
        osk.set_focused(true);
        assert!(osk.is_open(), "summoning it did not undo the refusal");
    }

    #[test]
    fn login_redraws_and_next_fields_respect_a_physical_keyboard() {
        let _held = alone();
        let mut osk = Osk::default();
        osk.set_controller_in_hand(false);
        assert!(!osk.offer_shell_field(true));
        assert!(!osk.is_open());
        for _ in 0..10 {
            assert!(!osk.offer_shell_field(true));
        }
        osk.offer_shell_field(false);
        assert!(
            !osk.offer_shell_field(true),
            "next login field must remember physical typing"
        );
        assert!(osk.keyboard_at_hand);
    }

    #[test]
    fn typing_after_controller_login_does_not_reopen_the_board() {
        let _held = alone();
        let mut osk = Osk::default();
        osk.set_controller_in_hand(true);
        assert!(osk.offer_shell_field(true));
        assert!(osk.types_here(), "shell input needs no virtual keyboard");
        osk.set_controller_in_hand(false);
        // The first grabbed letter redraws the panel before on_typed closes the board.
        assert!(!osk.offer_shell_field(true));
        osk.close();
        for _ in 0..10 {
            assert!(!osk.offer_shell_field(true));
            assert!(!osk.is_open());
        }
        osk.offer_shell_field(false);
        assert!(!osk.offer_shell_field(true));
        assert!(!osk.is_open());
    }

    #[test]
    fn physically_typing_after_manually_reopening_a_login_remembers_the_keyboard() {
        let _held = alone();
        let mut osk = Osk::default();
        osk.set_controller_in_hand(false);
        osk.offer_shell_field(true);
        osk.open_here();
        osk.dismiss_for_typing();
        osk.offer_shell_field(false);
        assert!(
            !osk.offer_shell_field(true),
            "a later password or Steam Guard field must stay closed"
        );
        assert!(osk.keyboard_at_hand);
    }

    #[test]
    fn login_keyboard_dismissal_persists_until_requested_again() {
        let _held = alone();
        let mut osk = Osk::default();
        osk.set_controller_in_hand(true);
        assert!(osk.offer_shell_field(true));
        osk.close();
        assert!(
            !osk.offer_shell_field(true),
            "refresh must not undo manual dismissal"
        );
        assert!(osk.open_here(), "explicit request still opens it");
        assert!(osk.types_here());
        assert!(
            osk.offer_shell_field(false),
            "leaving the login closes its board"
        );
        assert!(
            osk.offer_shell_field(true),
            "a new controller login offers it again"
        );
    }

    /// The same refusal, carried in from outside — which is how it survives a
    /// session. The shell watches for typing the board itself cannot see, and
    /// remembers which control it last saw; a user who spent the evening typing
    /// must not have a keyboard thrown over the first text field of the
    /// morning.
    #[test]
    fn a_board_told_the_keyboard_is_in_hand_stops_offering_itself() {
        let _held = alone();
        let mut osk = armed();
        osk.set_controller_in_hand(false);

        osk.set_focused(true);
        assert!(
            !osk.is_open(),
            "a keyboard came up over the shoulder of somebody typing"
        );
        // And the corner chip is still wanted, because that answer belongs to
        // the shell: what it draws over an application is decided beside every
        // other thing that is, in `keyboard_is_visible`.
        assert!(osk.wants_hint());

        // A thumb lands on the pad. The next field is offered a board again.
        osk.set_controller_in_hand(true);
        osk.set_focused(false);
        osk.set_focused(true);
        assert!(osk.is_open(), "the controller asked and was refused");

        // And asking for it outright is never refused, whichever hand asks: a
        // keyboard user who presses the binding wants the board.
        osk.close();
        osk.set_controller_in_hand(false);
        assert!(osk.open());
        assert!(osk.is_open());
    }

    /// A dismissed board leaves the way it arrived. The keys go back to the
    /// application on the instant — a quarter of a second unable to type would
    /// be the board's parting insult — but the slab is still on screen, and
    /// everything that draws has to keep saying so until it is not.
    #[test]
    fn the_board_slides_out_of_the_way_rather_than_vanishing() {
        let _held = alone();
        // A frame at sixty, which is roughly what the shell draws at.
        const FRAME: f32 = 1.0 / 60.0;
        let mut osk = armed();
        assert!(!osk.is_on_screen(), "nothing has summoned it");

        osk.open();
        while osk.animate(FRAME) < 1.0 {}
        assert!(osk.is_on_screen());

        osk.close();
        assert!(!osk.is_open(), "the keys go back at once");
        assert!(
            osk.is_on_screen(),
            "and the board is still there to be drawn"
        );

        // It takes as long to leave as it took to arrive, and only then is it
        // gone. A board that stopped being drawn on the keystroke would be the
        // vanishing this exists to prevent.
        let mut frames = 0;
        while osk.animate(FRAME) > 0.0 {
            assert!(osk.is_on_screen(), "gone before it finished leaving");
            frames += 1;
            assert!(frames < 1000, "the board never landed");
        }
        assert!(frames > 1, "it left in a single frame");
        assert!(!osk.is_on_screen());
    }

    /// The guide is the exception, because it is not this screen with the
    /// board taken off it. It covers the space the board would fall through,
    /// so there is nothing for the fall to be seen against — and a menu shut
    /// again straight away must not hand back a keyboard caught halfway out.
    #[test]
    fn the_guide_taking_the_screen_takes_the_board_with_it() {
        let _held = alone();
        let mut osk = armed();
        osk.open();
        osk.animate(1.0);
        assert!(osk.is_on_screen());

        osk.dismiss_at_once();
        assert!(!osk.is_open());
        assert!(
            !osk.is_on_screen(),
            "the board was left mid-fall under the menu"
        );
    }

    /// One number in both directions, so that a board asked for again while
    /// the last one is still leaving comes back up from where it is. Restarting
    /// the rise would drop it to the bottom edge first, which reads as two
    /// keyboards rather than one changing its mind.
    #[test]
    fn a_board_summoned_mid_fall_comes_back_from_where_it_is() {
        let _held = alone();
        const FRAME: f32 = 1.0 / 60.0;
        let mut osk = armed();
        osk.open();
        while osk.animate(FRAME) < 1.0 {}

        osk.close();
        osk.animate(FRAME);
        let halfway = osk.animate(FRAME);
        assert!(halfway > 0.0 && halfway < 1.0);

        osk.open();
        assert!(
            osk.animate(FRAME) > halfway,
            "it fell back to the edge before rising again"
        );
    }

    /// Shift is applied to the keysym before the shell ever sees it, so
    /// passing it on as well would shift the letter twice. Everything else has
    /// to go through, or Ctrl+C would stop working while the board was up.
    #[test]
    fn the_modifiers_passed_on_are_the_ones_the_keysym_did_not_already_carry() {
        let _held = alone();
        const SHIFT: u32 = 0x1;
        const LOCK: u32 = 0x2;
        const CONTROL: u32 = 0x4;
        const ALT: u32 = 0x8;

        let held = Modifiers {
            depressed: SHIFT | CONTROL,
            latched: 0,
            locked: LOCK,
            group: 0,
        };
        let passed = held.forwarded();
        assert_eq!(passed.depressed, CONTROL, "shift was sent twice");
        assert_eq!(passed.locked, 0, "caps lock was sent twice");

        assert_eq!(
            Modifiers {
                depressed: CONTROL | ALT,
                ..Default::default()
            }
            .forwarded()
            .depressed,
            CONTROL | ALT
        );
        // Nothing held is the common case, and it must stay cheap: no
        // modifier request is made around a plain letter at all.
        assert!(Modifiers::default().forwarded().is_empty());
        assert!(!held.forwarded().is_empty());
        assert!(Modifiers {
            depressed: SHIFT,
            ..Default::default()
        }
        .forwarded()
        .is_empty());
    }

    #[test]
    fn a_keyboard_with_nothing_to_type_with_never_opens_by_itself_either() {
        let _held = alone();
        let mut osk = Osk::default();
        osk.set_focused(true);
        assert!(!osk.is_open(), "a keyboard that cannot type came up anyway");
        assert!(!osk.wants_hint());
    }
}
