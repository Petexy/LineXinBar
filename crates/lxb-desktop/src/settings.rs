//! The Settings column: the shell's own rows, rather than anything installed.
//!
//! Everything else in the bar is a consequence of what is on disk — a
//! `.desktop` file sorted into a column. This one is written here, because
//! what it holds are LineXinBar's own controls, and a shell that had to find its
//! own settings on the filesystem could be left without them.
//!
//! It is a tree, and deliberately: a console settings region is a short column
//! of subcategories rather than one long list, so a setting is always two or
//! three rows away instead of thirty. Each level here is a [`Folder`], which is
//! what the bar steps into.
//!
//! A row that sets something carries a [`Setting`] saying what. The bar knows
//! how to move a mark from one row of a column to another — that is
//! [`Cursor::choose`] — and nothing more; what the value *means* is applied
//! here, in [`apply`], which is also where it is written down so the next
//! session comes up the way this one was left.
//!
//! Three kinds of setting live here, and no two of them are applied the same
//! way. The accent is the shell's own and takes effect in the next frame it
//! draws. Everything under Display belongs to the *compositor* — a colour
//! pipeline on a CRTC and an infoframe on a connector, neither of which a
//! client may touch — so what this module does with it is record it, and `main`
//! sends it over `lxb_shell_v1` for the compositor to carry out.
//!
//! The Display settings are also *per display*, all the way down: one screen
//! can be an HDR television and the next a laptop panel, and the two want
//! different answers. So the tree names the screen before it offers a setting,
//! and every [`Setting`] under Display carries which screen it belongs to.
//!
//! The third kind is the machine's own sound devices, under Sounds, and they
//! are the outlier: they belong to the *sound server*, which is neither this
//! shell nor the compositor, and which outlives both. So they are neither
//! carried out nor written down here — `main` hands the choice to
//! [`crate::system`], and the server remembers it for every application on the
//! machine. What this module keeps of them is what it keeps of a display's
//! modes: the last listing, so the page can be drawn.
//!
//! The fourth is Network, and it is the third kind again rather than a new one:
//! what is on the other end of it is `NetworkManager`, which is neither this
//! shell nor the compositor and outlives both, so `main` hands the press to
//! [`crate::network`] and the daemon remembers it. It is only listed separately
//! because the tree gets more from it than a listing — a press there can come
//! back wanting a password, which is a question this module publishes and the
//! shell puts on screen.
//!
//! [`Cursor::choose`]: crate::model::Cursor::choose

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::apps::{Category, Choice, Entry, Folder};
use crate::icons;
use crate::layouts;
use crate::system::{Devices, Direction, Level};
use crate::theme::{self, Color};
use lxb_protocol::pip;
use lxb_protocol::wallpaper;

/// What choosing a row does.
///
/// The row that carries it is generic — a column of values looks like any
/// other column — so the shell has to be able to tell one list of values from
/// the next without reading their titles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// Set the accent to the palette of this name — one of [`theme::ACCENTS`].
    Accent(&'static str),
    /// Draw one half of the shell — the wallpaper, or every mark it makes — in
    /// the material of this name, one of [`lxb_protocol::wallpaper::STYLES`].
    ///
    /// The two halves carry the [`theme::Part`] they are about rather than
    /// being two variants, because they are one question asked twice and
    /// everything that answers it answers both the same way.
    ///
    /// The one setting here that is about what the machine can *afford* rather
    /// than about what the user wants to look at, which is why its rows say so
    /// in their comments — though it is a preference too, and that is the whole
    /// reason the two are separate: a machine that cannot pay for the water
    /// behind everything can very well pay for the marks in front of it.
    Style(theme::Part, &'static str),
    /// One press on one of the two forms under Settings > Users — see
    /// [`UserValue`], which is where the whole of that page's oddity is argued.
    ///
    /// Carried out by the shell rather than by [`apply_with`], on the terms
    /// [`Setting::Network`] is: what has to happen is a draft being written or a
    /// daemon being asked, and `settings` records what the shell *remembers*,
    /// which is neither of those.
    User(UserValue),
    /// One press on Settings > Games > Steam — see [`SteamValue`], which is
    /// where each of the three is argued.
    ///
    /// Recorded here and carried out by the shell, on the terms
    /// [`Setting::Network`] is: what has to happen is a worker being started or
    /// stopped, a bar being built again and Valve's client being asked to shut
    /// down, and none of those is a thing this module does.
    Steam(SteamValue),
    /// Set one of an emulator core's own settings — PPSSPP's rendering
    /// resolution, Mesen's overclock.
    ///
    /// Nothing about these is written down in this shell. The core declares
    /// what it can be set to, the helper asks it, and the rows are built from
    /// the answer — see [`crate::retroarch::tunables`]. What is carried here is
    /// only enough to write the chosen value into the file the emulator reads
    /// it from: which core, which key, which value.
    ///
    /// `core` is what the core calls *itself* — `PPSSPP` — because that is the
    /// name RetroArch files its settings under, and not the `ppsspp` that names
    /// its file on the disk.
    CoreOption {
        core: &'static str,
        key: &'static str,
        value: &'static str,
    },
    /// Set one of RetroArch's own settings, which belong to no core: the
    /// aspect every game is drawn at, the driver it draws with.
    ///
    /// Written into the emulator's own configuration and kept, unlike the
    /// controller order — see [`crate::retroarch::set_setting`], which is where
    /// that difference is argued.
    Emulator {
        key: &'static str,
        value: &'static str,
    },
    /// Fetch the cover and the screenshot of every game in somebody's ROM
    /// folder again, from libretro's collection.
    ///
    /// The one row in this tree that sets nothing at all: it carries no value,
    /// nothing is marked afterwards, and pressing it twice does the same thing
    /// twice. It is here rather than in a menu because that is where somebody
    /// goes looking for it — the covers are a thing about the whole collection,
    /// and the collection's other settings are on this page.
    ///
    /// Applied by the shell rather than by [`apply_with`], on the terms
    /// [`Setting::Network`] is: what has to happen is a helper process being
    /// run, and this module writes down what the shell remembers.
    EmulatorArt,
    /// Play the Start screen's background music, or leave that screen quiet.
    ///
    /// Carries no display, unlike everything under Display: the music belongs
    /// to the session rather than to a screen — see [`crate::sound`] — so it is
    /// on or off for the whole of it.
    StartMusic(bool),
    /// Write the battery's charge out in figures beside the clock, or show the
    /// level and nothing else.
    ///
    /// Carries no display, like the music above it: the corner is drawn on
    /// every screen, and a number that appeared on one of them would be a
    /// second answer to a question the machine has one of.
    ///
    /// Reaches no hardware and is the shell describing itself, which is why it
    /// is under Appearance and not under System. The row exists only on a
    /// machine that has a battery — see [`battery_percent_switch`], and
    /// [`note_battery`], which is what says so.
    BatteryPercent(bool),
    /// Which column of the start screen the shell opens on.
    ///
    /// The id of the column, which is what a column is called on the bar and
    /// never what it is called on screen: a title is what the user reads and is
    /// entitled to change with their language, and this is written into a file
    /// that has to still mean the same column next year.
    ///
    /// Carries no display, like the switches around it. A session comes up on
    /// one place whichever screen is being looked at, and two screens opening
    /// on two different columns would be answering a question about a person
    /// with a fact about a monitor.
    ///
    /// Nothing is carried out when this is pressed. The value is what the next
    /// cursor to be made is built from — see [`crate::model::Cursor::for_model`]
    /// — so a press moves the mark, is written down, and is seen the next time
    /// a display comes up. That is the whole of it, and it is why the row says
    /// what it is for rather than appearing to do nothing.
    StartupCategory(&'static str),
    /// Whether this shell writes what its buttons do, anywhere it has room to.
    ///
    /// Carries no display, like the two above it: a legend is drawn on
    /// whichever screen is being driven, and a session where one screen
    /// explained the buttons and the other did not would be answering a
    /// question about a person with a fact about a monitor.
    ///
    /// Under System rather than under Appearance, which is the one thing about
    /// it worth arguing over. What it changes is not how the shell *looks* but
    /// how much it says about itself — the same kind of answer as how large an
    /// application is drawn, which is the row above it, and not the same kind
    /// as an accent colour. See [`button_hints`].
    ButtonHints(bool),
    /// Draw every application this much larger than life, in per cent of its
    /// own size. 100 is one to one, and the least this can be.
    ///
    /// Carries no display, unlike everything under Display, and deliberately:
    /// how large an interface has to be to be read is a fact about the person
    /// in front of the screens rather than about one of them.
    ///
    /// Set on a bar rather than chosen off a list, like the night light's
    /// temperature and for the same reason — what arrives here is one step
    /// along it. See [`Entry::Bar`].
    ///
    /// [`Entry::Bar`]: crate::apps::Entry::Bar
    AppScale(u16),
    /// Send everything the machine plays to this device from now on, or take
    /// everything it records from it.
    ///
    /// The one setting in this tree that is not the shell's own. The accent is
    /// LineXinBar's, the Display settings are the session's compositor's, and this
    /// is the *machine's* — every application on it, whether or not this shell
    /// is running when they start. So it is also the one the shell does not
    /// write down: see [`apply_with`], where it is applied by handing it to the
    /// sound server, which is what remembers it.
    SoundDevice {
        direction: Direction,
        /// The sound server's own name for the device — `alsa_output.…` —
        /// interned for the reason a connector name is. See [`intern`].
        id: &'static str,
    },
    /// Change what this machine is on: the radio, a wireless network, or the
    /// socket in the back.
    ///
    /// The second setting in this tree that is not the shell's own and is not
    /// written down here — see [`Setting::SoundDevice`], which is the first and
    /// is the same bargain. What is on the other end of it is
    /// `NetworkManager`, which is what remembers a network once it has been
    /// joined; a shell that kept its own copy would be a second opinion about
    /// the machine's network at every login, and every other program on the
    /// machine would be looking at the first one.
    ///
    /// Carried out by the caller for the reason the sound device is: it goes
    /// over D-Bus to a daemon, and a module that held that connection could not
    /// be tested on a machine that has none. See [`crate::network`].
    Network(NetworkValue),
    /// Change what this machine is paired with: the controller, or one of the
    /// things it can hear.
    ///
    /// The third setting in this tree that is not the shell's own and is not
    /// written down here, and the same bargain as the two above it. What is on
    /// the other end of it is BlueZ, which is what remembers a bond once it has
    /// been made; a shell that kept its own copy would be a second opinion
    /// about what this machine is paired with, and every other program on it —
    /// the game reading the controller, the mixer showing the headset — would
    /// be looking at BlueZ's.
    ///
    /// Carried out by the caller for the reason the network is. See
    /// [`crate::bluetooth`].
    Bluetooth(BluetoothValue),
    /// Change what happens to a browser's picture-in-picture window: whether it
    /// floats over everything, how large it is drawn, and which corner it sits
    /// in.
    ///
    /// Carried out by the compositor and recorded here, which is the bargain
    /// [`Setting::AppScale`] is under and for the same two reasons: where a
    /// window goes is not the shell's to do, and a module holding a Wayland
    /// connection could not be tested without one. `main` sends whatever
    /// [`picture_in_picture`] then returns.
    ///
    /// Carries no display. The window floats on the screen its browser is on,
    /// and the setting is about what such a window *is* rather than about any
    /// one screen — the same argument the application scale is under.
    PictureInPicture(PipValue),
    /// Change what a mouse does: how fast the pointer travels, how large it is
    /// drawn, and how far and which way a wheel carries the content under it.
    ///
    /// Recorded here and carried out by the compositor, which is the bargain
    /// [`Setting::AppScale`] is under and for the same two reasons twice over:
    /// three of the four are libinput settings on a device only the compositor
    /// opens — the shell never holds the seat — and the fourth is the size of a
    /// picture the compositor is the one drawing. `main` sends whatever
    /// [`pointer`] then returns.
    ///
    /// Carries no display, like the application scale: how fast a hand has to
    /// move to cross the desk is a fact about the desk and the person at it,
    /// not about either of the screens on it.
    Pointer(PointerValue),
    /// Which display the on-screen keyboard comes up on: a connector by name,
    /// or `None` for whichever screen is being driven.
    ///
    /// The one setting in this tree whose *value* is a display while the
    /// setting itself is not about one. Everything under
    /// [`Setting::Display`] is a property of a screen and is filed under that
    /// screen; this is a property of the *board* — there is one of it in the
    /// session, and it is on one screen at a time — which happens to be
    /// answered by naming a screen. So it is session-wide, and it carries the
    /// connector as its value rather than beside it.
    ///
    /// Nothing outside the shell is told. Which display draws the board is
    /// decided every frame by whatever is about to draw one — see
    /// `Shell::keyboard_panel` — so the press moves the mark, this writes it
    /// down, and the next board comes up where it was asked for.
    ///
    /// Interned, like every connector name in this module. See [`intern`].
    KeyboardDisplay(Option<&'static str>),
    /// Which arrangement every keyboard on this machine is set to, as
    /// `layout (variant)` — the form `setxkbmap -query` prints. See
    /// [`crate::layouts::Layout::key`].
    ///
    /// Recorded here and carried out by the compositor, which is the bargain
    /// [`Setting::Pointer`] is under and for its first reason: the seat belongs
    /// to the half of the session that opens input devices, and the shell has
    /// never held a keyboard. `main` sends whatever [`keyboard_layout`] then
    /// returns.
    ///
    /// Session-wide, and there is one of it. xkb can hold several layouts at
    /// once with a key to walk between them; this page asks one question and
    /// that is a second question, with a shortcut of its own to settle before
    /// it could be asked.
    ///
    /// Interned, like every value in this module that is a name. See
    /// [`intern`].
    KeyboardLayout(&'static str),
    /// Change one display's picture. Carries the connector the change belongs
    /// to, because every one of these is a property of one screen.
    Display {
        display: &'static str,
        value: DisplayValue,
    },
}

/// One thing that can be changed about Steam.
///
/// Three questions about one program, and they are three rather than one
/// because they are answered at three different moments: whether this shell
/// has anything to do with Steam at all, what it does as the session comes up,
/// and what it does when a game is over. Only the first changes what is on the
/// bar; the other two change how long somebody waits for a game to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteamValue {
    /// Whether this shell drives Valve's client at all.
    ///
    /// **On.** A console that plays Steam games is what most of this shell's
    /// Games half is for, and the integration is the whole of it: the row at
    /// the head of Games that signs an account in, the column of that
    /// account's library, the artwork behind it, the download card in the
    /// guide, and a game that starts from a tile rather than out of a
    /// storefront.
    ///
    /// Off, none of that exists and Steam is an application like any other:
    /// its own `.desktop` entry stands on the bar where the scan filed it,
    /// wearing the icon its package ships, and starting it puts Valve's own
    /// window on the screen. Which is the right answer for a machine where
    /// somebody else's account is signed in, for one where Steam is used from
    /// a desk with a mouse, and for one where the whole of this integration is
    /// simply not wanted.
    ///
    /// It is the same session-wide switch `--no-steam` throws, and the flag
    /// still outranks it: a session started with Steam left out has no page
    /// here to press. See [`steam_in_this_session`].
    Integration(bool),
    /// Whether Valve's client is started in the background as the shell comes
    /// up, rather than when the first game is pressed.
    ///
    /// **Off.** The client is a few hundred megabytes of resident memory and a
    /// long cold start, and a machine whose owner spent the evening watching a
    /// film should not have paid for either. On, the wait is paid once while
    /// nobody is looking rather than in front of the first loading screen of
    /// the day.
    AtStartup(bool),
    /// Whether Valve's client is left running once a game has ended.
    ///
    /// **On**, which is what this shell has always done: a client started for
    /// one game is still up for the next, and the second game of an evening
    /// starts in a second or two rather than in twenty. Off, it is asked to
    /// shut down when the last Steam game's window has gone — the memory comes
    /// back, and the next game pays the cold start again.
    ///
    /// Never while it is in the middle of something. See
    /// [`crate::steam::Steam::nothing_is_under_way`].
    AfterAGame(bool),
    /// Which Steam Play compatibility tool runs the games Valve has not
    /// verified — Steam's own default for "all other titles" — by the name
    /// Steam files it under, or nothing at all.
    ///
    /// **Nothing**, which is Steam's own default and this shell's: a machine
    /// where nobody has chosen runs verified games under whatever Valve says
    /// and refuses to run the rest, which is what Steam does out of the box.
    ///
    /// The one value in this tree that is not this shell's to keep. It is
    /// written into Valve's own configuration, it is the same setting the
    /// client's own Compatibility page sets, and it applies to Steam whether
    /// this shell is running or not — so it is never written to the settings
    /// file, and what the row draws is always what Steam last said rather than
    /// what this session remembers. See [`note_compatibility_tools`].
    OtherTitles(Option<&'static str>),
}

/// One thing that can be changed about the floating window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipValue {
    /// Float such a window at all. Off, a window a browser has put a video into
    /// is an application window like any other — maximized, listed in the
    /// guide, and given the keyboard.
    Floating(bool),
    /// How much of the display's width it takes.
    Size(pip::Size),
    /// Which corner it sits in.
    Place(pip::Place),
}

/// One thing that can be changed about a display's picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayValue {
    /// Scan this display out at this many pixels.
    Resolution(Resolution),
    /// Refresh it this many times a second, in mHz, at whatever size it is
    /// already being scanned out at.
    RefreshRate(u32),
    /// Draw its picture turned this way, for a screen standing on its side.
    Orientation(Orientation),
    /// Put this display at this place in the arrangement, counted from zero.
    ///
    /// The one value in this tree that is about two displays: the screen
    /// already standing there trades places with this one, because a list of
    /// screens has no empty places to move into. See [`display_order`].
    Place(u32),
    /// Drive this display in high dynamic range, or stop.
    Hdr(bool),
    /// The luminance plain white is sent at while HDR is on, in cd/m².
    SdrBrightness(u16),
    /// How far sRGB's colours are stretched towards BT.2020's, 0 to 100.
    SrgbIntensity(u8),
    /// Peak luminance declared to the display, in cd/m². 0 asks for whatever
    /// the display says about itself.
    PeakBrightness(u16),
    /// Run the night light on this display, or stop.
    NightLight(bool),
    /// How warm the night light makes the picture, in kelvin. Lower is warmer.
    ///
    /// The one setting in this tree that is not chosen off a list: it is set on
    /// a bar, so what arrives here is one step along it. See [`Entry::Bar`].
    ///
    /// [`Entry::Bar`]: crate::apps::Entry::Bar
    NightLightTemperature(u16),
    /// Which hours the light keeps: none, the sun's, or the two below.
    NightLightSchedule(Schedule),
    /// The hour of local time it comes on at, 0 to 23.
    NightLightFrom(u8),
    /// The hour of local time it goes off again, 0 to 23.
    NightLightUntil(u8),
    /// Rest this display behind black once it has been left alone, while
    /// another one is being used.
    ///
    /// Per display like everything else here, and it has to be: what this
    /// protects against is a panel keeping the still picture it is shown, and
    /// whether a screen does that is a fact about the screen. A television and
    /// an OLED handheld beside it are not the same question.
    OledProtection(bool),
}

/// One thing that can be changed about the session's pointing devices.
///
/// Every one of them is about all of them: a mouse, a touchpad and a
/// controller's trackpad move the same pointer, and a setting that applied to
/// one of them would be a page that works on some desks and not others. So
/// none of these names a device, unlike a [`NetworkValue`], which names the
/// interface it belongs to — there is one pointer, and this is what it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerValue {
    /// libinput's acceleration, in hundredths: -100 to 100, 0 being the flat
    /// default every desktop starts at.
    ///
    /// Passed through rather than translated into something friendlier. What
    /// libinput does with the number depends on the device's own resolution, so
    /// a shell that turned it into "pixels per inch" would be inventing a
    /// second scale that agrees with the first on one mouse and no other.
    Speed(i8),
    /// The cursor's nominal size in logical pixels, as `XCURSOR_SIZE` counts
    /// it.
    Size(u16),
    /// How far one movement of a wheel carries the content under it, in per
    /// cent of what the device reported. 100 is one to one.
    ///
    /// Not libinput's — it has none — so this one is arithmetic the compositor
    /// does over the movements it forwards. Named a speed rather than a
    /// distance because that is what it is called everywhere else, and because
    /// what a user is changing is how fast a page goes by.
    Scroll(u16),
    /// Whether the content follows the fingers — the direction a touchscreen
    /// moves — or the wheel's traditional direction.
    Natural(bool),
}

/// One thing that can be changed about what this machine is on.
///
/// Every one of these names the device it belongs to, for the reason every
/// [`DisplayValue`] names a screen: a laptop in a dock has two sockets and a
/// radio, and "connect" is not a question the machine has one answer to. The
/// radio switch is the exception and carries no device, because
/// `NetworkManager` has one switch for every radio in the machine — it is a
/// property of the manager, not of an interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkValue {
    /// Turn the wireless radio on, or off.
    Radio(bool),
    /// Put this device on the network of this name.
    ///
    /// The name in the air rather than the access point it was heard from, and
    /// deliberately: a house with a repeater in it publishes one name from two
    /// radios, they are one row on the page, and which of them to use is a
    /// question the *radio* answers better than the shell does. See
    /// [`crate::network::Network`].
    Join {
        device: &'static str,
        ssid: &'static str,
    },
    /// Take this device off whatever it is on.
    Leave { device: &'static str },
    /// Delete the saved profile for this network on this device.
    ///
    /// The one row under Network that removes something rather than setting
    /// something, and the only press in the whole Settings tree that a user
    /// cannot undo from the page they are standing on: what goes is the key
    /// this machine got on with. The row it is carried by says so — see
    /// [`action`] — because there is nothing between the press and the act.
    Forget {
        device: &'static str,
        ssid: &'static str,
    },
    /// Bring a wired socket up on a saved profile, or take it down.
    Wire { device: &'static str, up: bool },
    /// Take this interface's address from the network, or pin the one it has.
    Addressing {
        device: &'static str,
        automatic: bool,
    },
    /// Take its name servers from the network, or use the ones it names.
    ///
    /// Spelt as the screen spells it — see [`crate::network::Field::Dns`].
    Dns {
        device: &'static str,
        automatic: bool,
    },
}

/// One thing that can be changed about what this machine is paired with.
///
/// Every one of these names what it is about, for the reason every
/// [`NetworkValue`] names a device: a machine with two controllers in it has
/// two answers to "is Bluetooth on", and a press on a pair of headphones is
/// about those headphones and nothing else.
///
/// Pairing is deliberately not one of them: pressing a device the machine has
/// never met means *connect to that*, and whether a bond has to be made first
/// is BlueZ's answer rather than the user's — see
/// [`crate::bluetooth::Bt::connect`], which is where the two become one press.
///
/// Two of them are the shell's own and are written down; the rest are BlueZ's
/// and are handed over. Which is which is not arbitrary — see [`apply_with`]:
/// what BlueZ can answer, BlueZ keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BluetoothValue {
    /// Turn one controller on, or off.
    Power {
        /// BlueZ's own handle on it, interned for the reason a connector name
        /// is — see [`intern`].
        controller: &'static str,
        on: bool,
    },
    /// Connect to this device, pairing with it first if this machine never has.
    Connect { device: &'static str },
    /// Take it off this machine, and keep the pairing.
    Disconnect { device: &'static str },
    /// Remove the pairing from the machine.
    ///
    /// The second row in this tree that removes something rather than setting
    /// something — see [`NetworkValue::Forget`], which is the first and is the
    /// same act on a different object. What goes is the key this machine got on
    /// with, and the row that carries it says so, because there is nothing
    /// between the press and the act.
    Forget { device: &'static str },
    /// Let anything nearby find this machine, or only what it already knows.
    Visible { controller: &'static str, on: bool },
    /// Make this controller the one the machine's Bluetooth *is*.
    ///
    /// By address rather than by path, because what is being remembered has to
    /// survive the kernel renumbering the adapters. See
    /// [`BLUETOOTH_CONTROLLER`].
    Use { address: &'static str },
    /// What happens to Bluetooth when a session starts.
    Startup(Startup),
}

/// One press on the two forms under Settings > Users.
///
/// The odd one out in this whole enum, and worth saying why. Every other
/// [`Setting`] here *is* the change — pressing it moves a mark and something
/// happens. Three of these four move nothing at all: they write into a draft
/// held in [`crate::users`], and the account is not touched until the row at the
/// foot of the form is pressed. That is what a form is, and it is the reason the
/// draft exists rather than each row acting on its own.
///
/// They are settings all the same because of what carries them: the bar knows
/// how to move a mark from one row of a column to another and nothing else, and
/// the Account type rows are exactly that — a column of two answers, one of
/// which is in force. Making them a different kind of row would be writing that
/// column's behaviour a second time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserValue {
    /// Which kind of account the form is for. The one row of the form that is
    /// chosen from a list rather than typed or walked to.
    Admin(bool),
    /// Hand the form over: make the account, or save the changes.
    Accept,
    /// Take the chosen picture back off, so the row wears the figure again.
    DropPicture,
    /// Ask whether to remove this account. The question, not the answer — see
    /// [`crate::menu::Command::ConfirmUninstall`], which is the same division:
    /// one of these opens a panel and something else destroys an account.
    Remove(u64),
}

/// One of the values in this tree that is typed rather than chosen: which one,
/// and whose.
///
/// Carried by [`Entry::Typed`] rather than by a [`Setting`], because it is not
/// one: a setting is a thing a press *applies*, and pressing one of these opens
/// a field. What arrives back is the text, and the shell hands the pair to
/// whichever module owns it — the same division every other row on these pages
/// is under, where what this module does is describe the row and somebody else
/// carries it out.
///
/// [`Entry::Typed`]: crate::apps::Entry::Typed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typing {
    /// One of a network interface's addressing values — an address, a router,
    /// a list of name servers.
    Network {
        /// The interface it belongs to, interned for the reason a connector
        /// name is — see [`intern`].
        device: &'static str,
        field: crate::network::Field,
    },
    /// What this machine calls itself over Bluetooth.
    ///
    /// The one value in this tree that is typed and is not an address, and the
    /// only one that anybody but this machine ever sees: it is the name a phone
    /// looking for something to pair with puts on its own screen. See
    /// [`crate::bluetooth::Bt::rename`].
    BluetoothName { controller: &'static str },
    /// One of the values typed into an account form — a name, a user name, or
    /// one of the two passwords.
    ///
    /// It carries *whose* as well as which, and that is not decoration: two of
    /// the checks a field makes depend on it. A user name is taken or free for
    /// this account, and a password may be left empty on one that already exists
    /// and may not on one that does not. See [`crate::users::fault`].
    User {
        whose: crate::users::Whose,
        field: crate::users::Field,
    },
}

impl Typing {
    /// What to type, for somebody looking at an empty field on a television.
    pub fn note(self) -> &'static str {
        match self {
            Typing::Network { field, .. } => field.note(),
            Typing::BluetoothName { .. } => {
                "What other devices call this machine when they look for it."
            }
            Typing::User { field, .. } => field.note(),
        }
    }

    /// Whether the field is drawn as a count of marks rather than as what was
    /// typed.
    ///
    /// True for exactly two rows in the tree, and they are the only two values
    /// the shell collects that are *secrets*. Everything else typed here is an
    /// address or a name — see [`crate::dialog::Line::Entry`], which is where
    /// the argument for showing those is made, and which this is the exception
    /// to rather than the rule for.
    pub fn secret(self) -> bool {
        match self {
            Typing::Network { .. } | Typing::BluetoothName { .. } => false,
            Typing::User { field, .. } => field.secret(),
        }
    }

    /// What is wrong with what was typed, or nothing if it will do.
    ///
    /// Asked while the panel is still on screen and can still say so, which is
    /// the whole reason this is here rather than in the module that carries the
    /// value out: a field that accepted anything and failed silently a second
    /// later is a field with no answer to "why did nothing happen".
    pub fn fault(self, text: &str) -> Option<&'static str> {
        match self {
            Typing::Network { field, .. } => crate::network::fault(field, text),
            Typing::BluetoothName { .. } => crate::bluetooth::fault(text),
            Typing::User { whose, field } => crate::users::fault(whose, field, text),
        }
    }
}

/// What happens to Bluetooth when a session starts.
///
/// Three answers rather than a switch, because "on" and "off" between them
/// cannot say the thing most people actually want, which is *leave it*. A
/// machine that is only ever paired with a controller wants it on; a machine
/// where Bluetooth is a battery cost wants it off; and somebody who turns it on
/// for an evening and off again wants neither of those decided for them at every
/// login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Startup {
    /// Turned on as the session comes up.
    On,
    /// Turned off as the session comes up.
    Off,
    /// Put back the way the last session left it.
    ///
    /// The default, and the only one of the three that changes nothing about a
    /// machine nobody has been to this page on. What it restores is what *this
    /// shell* last saw — see [`note_bluetooth_powered`] — rather than what BlueZ
    /// happens to remember, because a machine where the two disagree is one
    /// where the user last pressed the switch on this page.
    #[default]
    Restore,
}

impl Startup {
    /// How the settings file spells it.
    fn key(self) -> &'static str {
        match self {
            Startup::On => "on",
            Startup::Off => "off",
            Startup::Restore => "restore",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        match key {
            "on" => Some(Startup::On),
            "off" => Some(Startup::Off),
            "restore" => Some(Startup::Restore),
            _ => None,
        }
    }

    /// What the row above the three says.
    fn title(self) -> &'static str {
        match self {
            Startup::On => "On",
            Startup::Off => "Off",
            Startup::Restore => "As it was left",
        }
    }
}

/// When a night light burns.
///
/// Three answers rather than a switch and a pair of hours, because they are
/// three different things to want and the user has to be able to say which
/// without setting up the other two. A schedule kept while another one is in
/// force is not thrown away: choosing Hours again gives back the evening that
/// was there, and choosing the sun's again gives back the sun's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
    /// On for as long as the switch is, which is what somebody who wants it on
    /// while they work wants.
    AllDay,
    /// From sunset to sunrise where this machine is. The times come from the
    /// time zone's own coordinates — see [`crate::sun`] — and change every day
    /// without anybody setting anything.
    SunsetToSunrise,
    /// Between two hours of local time the user chose.
    Hours,
}

impl Schedule {
    /// How the settings file spells it.
    fn key(self) -> &'static str {
        match self {
            Schedule::AllDay => "all-day",
            Schedule::SunsetToSunrise => "sunset-to-sunrise",
            Schedule::Hours => "hours",
        }
    }

    /// The same, read back. `None` for a word this shell does not have, which
    /// a hand-edited file may hold and a later version may write.
    fn from_key(raw: &str) -> Option<Self> {
        Some(match raw.trim().to_ascii_lowercase().as_str() {
            "all-day" | "always" => Schedule::AllDay,
            "sunset-to-sunrise" | "sun" => Schedule::SunsetToSunrise,
            "hours" | "custom" => Schedule::Hours,
            _ => return None,
        })
    }

    /// What the row is titled with.
    fn title(self) -> &'static str {
        match self {
            Schedule::AllDay => "All day",
            Schedule::SunsetToSunrise => "Sunset to sunrise",
            Schedule::Hours => "Custom hours",
        }
    }
}

/// How many pixels a display is scanned out at: what a Resolution row sets.
///
/// The mode's own pixels, before any scale or transform — which is what the
/// connector lists and what the compositor matches against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

/// How a display's picture is turned.
///
/// The same eight the compositor has, which are `wl_output`'s own: four
/// rotations, and the four rotations of a mirrored picture. Only the rotations
/// are offered — see [`ROTATIONS`] — but all eight are named, because the
/// compositor's own config file can put a display into any of them and a page
/// that could not say what a display was doing would be worse than one that
/// cannot change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    Landscape,
    Portrait,
    LandscapeFlipped,
    PortraitFlipped,
    Mirrored,
    MirroredPortrait,
    MirroredFlipped,
    MirroredPortraitFlipped,
}

/// The four the page offers: the turns a screen can be stood on its side by.
///
/// The mirrored four are left out because a mirrored picture is not an
/// orientation anybody's screen is in — it is what a picture reaching the eye
/// through a mirror needs, which is a projector rig and not something to offer
/// on the way past. A display the config has put into one is still named, in
/// the row above the list.
pub const ROTATIONS: [Orientation; 4] = [
    Orientation::Landscape,
    Orientation::Portrait,
    Orientation::LandscapeFlipped,
    Orientation::PortraitFlipped,
];

impl Orientation {
    /// What a row of the list is titled with, and what the row above it says a
    /// screen is at.
    ///
    /// The turn itself, in degrees, rather than "Portrait" and "Landscape".
    /// Those two words name the *shape* that comes out, which is the one thing
    /// the row does not have to say — the drawing beside it is that shape. The
    /// number is what the row cannot show: which of the two portraits this is,
    /// and how far from where the display started.
    pub fn title(self) -> &'static str {
        match self {
            Orientation::Landscape => "0° Rotation",
            Orientation::Portrait => "90° Rotation",
            Orientation::LandscapeFlipped => "180° Rotation",
            Orientation::PortraitFlipped => "270° Rotation",
            Orientation::Mirrored => "Mirrored",
            Orientation::MirroredPortrait => "Mirrored, 90° rotation",
            Orientation::MirroredFlipped => "Mirrored, 180° rotation",
            Orientation::MirroredPortraitFlipped => "Mirrored, 270° rotation",
        }
    }

    /// The drawing beside it: one monitor, stood the way this turn stands it.
    ///
    /// `None` for the mirrored four, which are never a row — they are named in
    /// the row above a list they are not in. See [`ROTATIONS`].
    fn icon(self) -> Option<&'static str> {
        Some(match self {
            Orientation::Landscape => icons::SETTING_ROTATION_0,
            Orientation::Portrait => icons::SETTING_ROTATION_90,
            Orientation::LandscapeFlipped => icons::SETTING_ROTATION_180,
            Orientation::PortraitFlipped => icons::SETTING_ROTATION_270,
            _ => return None,
        })
    }

    /// The line under it: which way the screen it is meant for has been turned,
    /// since the title alone does not say which way a quarter turn goes.
    ///
    /// Said as where the screen's *top edge* ends up, rather than which edge it
    /// stands on: a user looking at a monitor knows where its top is, and the
    /// edge it is resting on is the one they cannot see. The turns are named
    /// from what the compositor actually draws — at `90` the picture is drawn a
    /// quarter turn anticlockwise, which stands up on a screen that has been
    /// turned clockwise.
    fn note(self) -> &'static str {
        match self {
            Orientation::Landscape => "Landscape, the way the display is built",
            Orientation::Portrait => "Portrait, for a screen turned clockwise",
            Orientation::LandscapeFlipped => "Landscape, for a screen hung upside down",
            Orientation::PortraitFlipped => "Portrait, for a screen turned the other way",
            // Not offered, and so never the title of a row that has a line
            // under it. Named for the row above the list, which prints the
            // title alone.
            _ => "Mirrored about a vertical axis",
        }
    }

    /// How the protocol counts them, which is how `wl_output` counts them.
    pub fn code(self) -> u32 {
        match self {
            Orientation::Landscape => 0,
            Orientation::Portrait => 1,
            Orientation::LandscapeFlipped => 2,
            Orientation::PortraitFlipped => 3,
            Orientation::Mirrored => 4,
            Orientation::MirroredPortrait => 5,
            Orientation::MirroredFlipped => 6,
            Orientation::MirroredPortraitFlipped => 7,
        }
    }

    /// The same, read back. `None` for a value this shell does not have, which
    /// a compositor built against a later protocol could send.
    pub fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            0 => Orientation::Landscape,
            1 => Orientation::Portrait,
            2 => Orientation::LandscapeFlipped,
            3 => Orientation::PortraitFlipped,
            4 => Orientation::Mirrored,
            5 => Orientation::MirroredPortrait,
            6 => Orientation::MirroredFlipped,
            7 => Orientation::MirroredPortraitFlipped,
            _ => return None,
        })
    }

    /// How the settings file spells it — which is how the compositor's own
    /// config spells it, for the reason the mode is spelled that way: one
    /// format across the two halves of the session, so a line can be moved
    /// between the files and mean the same thing.
    fn key(self) -> &'static str {
        match self {
            Orientation::Landscape => "normal",
            Orientation::Portrait => "90",
            Orientation::LandscapeFlipped => "180",
            Orientation::PortraitFlipped => "270",
            Orientation::Mirrored => "flipped",
            Orientation::MirroredPortrait => "flipped-90",
            Orientation::MirroredFlipped => "flipped-180",
            Orientation::MirroredPortraitFlipped => "flipped-270",
        }
    }

    /// The same, read back, accepting everything the compositor's config
    /// accepts. `None` for anything else, which a hand-edited file may hold.
    fn from_key(raw: &str) -> Option<Self> {
        Some(match raw.trim().to_ascii_lowercase().as_str() {
            "normal" | "0" => Orientation::Landscape,
            "90" => Orientation::Portrait,
            "180" => Orientation::LandscapeFlipped,
            "270" => Orientation::PortraitFlipped,
            "flipped" => Orientation::Mirrored,
            "flipped-90" | "flipped90" => Orientation::MirroredPortrait,
            "flipped-180" | "flipped180" => Orientation::MirroredFlipped,
            "flipped-270" | "flipped270" => Orientation::MirroredPortraitFlipped,
            _ => return None,
        })
    }
}

/// A whole mode: a size and a rate.
///
/// Two settings on the page and one thing on the wire, because a connector is
/// set to one mode rather than to a width and a rate separately — a display
/// asked for 1080p offers rates 1440p does not. So each of the two rows fills
/// in the half the other is not about, and what leaves here is always a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mode {
    pub resolution: Resolution,
    /// Refresh in mHz, as DRM reports it: a 59.94 Hz mode is 59940. 0 asks for
    /// the fastest the display offers at that size, which is what a size
    /// chosen on a display with no rate to carry over comes to.
    pub refresh: u32,
}

/// One mode a display offers, as the compositor lists them.
///
/// The two flags are not part of the [`Mode`] because they are not part of
/// what is being asked for: they say which row of the list to mark and which
/// one the display itself would pick, both of which are answers rather than
/// requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offered {
    pub mode: Mode,
    /// The display is being driven at this one now.
    pub current: bool,
    /// The display names this one as its own — its native timing.
    pub preferred: bool,
}

/// The whole of what the shell asks the compositor to do with one display.
///
/// One value rather than four settings, because it is applied as one: the
/// compositor rebuilds its LUTs and its matrix from all of it at once, so
/// sending a change to a single field still sends the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hdr {
    pub enabled: bool,
    pub sdr_brightness: u16,
    pub srgb_intensity: u8,
    /// 0 means "whatever the display says about itself".
    pub peak_brightness: u16,
}

impl Default for Hdr {
    fn default() -> Self {
        Self {
            enabled: false,
            // The compositor's own default, restated here so a shell reading
            // its settings before the compositor has said anything does not
            // come up asking for something different from what is in force.
            sdr_brightness: 200,
            srgb_intensity: 0,
            peak_brightness: 0,
        }
    }
}

/// The whole of one display's night light: whether it runs, how warm it makes
/// the picture, and between which hours.
///
/// One value rather than four settings for the reason [`Hdr`] is one: the shell
/// has to answer "is the light on *now*" out of all of it at once, and a
/// temperature held apart from the schedule that decides whether it is showing
/// would be two halves of an answer that nothing owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NightLight {
    /// The switch. Off means the display is never warmed, whatever the hours
    /// below say.
    pub enabled: bool,
    /// How warm the picture is made while it is on, in kelvin. Lower is
    /// warmer; [`NEUTRAL_KELVIN`] is ordinary daylight and no filter at all.
    pub temperature: u16,
    /// Which hours it keeps.
    pub schedule: Schedule,
    /// The hour of local time it comes on at, 0 to 23. Kept whatever the
    /// schedule says, so an evening set and then set aside comes back intact.
    pub from: u8,
    /// The hour it goes off again. Wrapping past midnight is the ordinary
    /// case, not the exception: an evening ends the next morning.
    pub until: u8,
}

impl Default for NightLight {
    fn default() -> Self {
        Self {
            enabled: false,
            // Warm enough to be worth switching on and mild enough that a
            // photograph is still recognisably the colour it was. Restated
            // from the compositor's own default so a shell that has read its
            // settings before the compositor has said anything is not asking
            // for something different from what is in force.
            temperature: 4000,
            // An evening, ready-made. A blue light filter is a thing people
            // want at night and not at eleven in the morning, so the switch
            // does what its name says the first time it is turned on and
            // nobody has to set two hours before it is worth having.
            //
            // It is the one default here that can look like nothing happening:
            // switched on in daylight it warms nothing until ten. That is what
            // the row's own comment is for — it reads "On at 22:00" rather than
            // "On", so the switch says which of the two it is doing.
            schedule: Schedule::Hours,
            from: 22,
            until: 6,
        }
    }
}

/// Ordinary daylight white: the temperature at which the filter is doing
/// nothing. The compositor treats it as exactly the identity, so it is the top
/// of the range this page offers rather than a row in it.
pub const NEUTRAL_KELVIN: u16 = 6500;

/// The warmest that can be asked for. Below it there is no blue left to take.
pub const WARMEST_KELVIN: u16 = 1000;

impl NightLight {
    /// Whether the light should be burning at this minute of local time, given
    /// what the sun is doing today.
    ///
    /// The whole of the schedule, and deliberately free of any clock and of any
    /// almanac: what time it is comes from [`local_time`] and what the sun does
    /// from [`crate::sun`], so everything that decides what to do about them
    /// can be asked a question and answered without either.
    ///
    /// Minutes rather than hours because the sun does not keep hours. A window
    /// the user typed is still whole hours — those are what the page offers —
    /// and it is turned into minutes on the way in.
    pub fn burning_at(self, minute: u16, sun: Option<crate::sun::Sun>) -> bool {
        if !self.enabled {
            return false;
        }
        match self.schedule {
            Schedule::AllDay => true,
            Schedule::Hours => Self::within(
                self.from as u16 * 60,
                self.until as u16 * 60,
                minute.min(MINUTES_IN_DAY - 1),
            ),
            Schedule::SunsetToSunrise => match sun {
                Some(crate::sun::Sun::Daily { sunrise, sunset }) => {
                    Self::within(sunset, sunrise, minute)
                }
                // A day the sun does not come up is a day that is night, and
                // one it does not go down is a day that is not. Both are the
                // truthful reading of "from sunset to sunrise" at a latitude
                // where neither happens.
                Some(crate::sun::Sun::NeverRises) => true,
                Some(crate::sun::Sun::NeverSets) => false,
                // Nothing knows where this machine is. The page does not offer
                // the sun where that is so, and a file that names it anyway is
                // put back to All day on the way in — so this is the belt to
                // that brace, and it errs towards the light being on, which is
                // what the switch above it says.
                None => true,
            },
        }
    }

    /// Whether `minute` falls in the window from `from` until `until`, both
    /// minutes of a day that wraps.
    ///
    /// The end is exclusive: a light set to go off at 07:00 is off at seven,
    /// not a minute past. The two being equal is an empty window rather than a
    /// full one — it cannot be chosen, because the Until page leaves the
    /// starting hour out, and a file hand-edited into it is dropped on the way
    /// in. See [`adopt`].
    fn within(from: u16, until: u16, minute: u16) -> bool {
        use std::cmp::Ordering;
        match from.cmp(&until) {
            // An ordinary daytime window: 07:00 to 21:00.
            Ordering::Less => (from..until).contains(&minute),
            // One that wraps past midnight, which is what an evening is — and
            // what sunset to sunrise always is.
            Ordering::Greater => minute >= from || minute < until,
            // Equal is the empty window, and has to be its own arm: the
            // wrapping test above would read it as every minute instead, which
            // is the one answer nobody asked for.
            Ordering::Equal => false,
        }
    }

    /// The next minute of the day at which this schedule changes its mind, if
    /// it changes it at all today.
    ///
    /// What the row above the page says out loud: *On until 07:00*, *On at
    /// 21:00*. Which end that is depends on which side of it the clock is, and
    /// the caller already knows that — this only has to say where the other
    /// side begins.
    ///
    /// `None` for a schedule with no edges: on all day, or a day at a latitude
    /// where the sun does not cross the horizon.
    fn next_edge(self, minute: u16, sun: Option<crate::sun::Sun>) -> Option<u16> {
        let (from, until) = match self.schedule {
            Schedule::AllDay => return None,
            Schedule::Hours => (self.from as u16 * 60, self.until as u16 * 60),
            Schedule::SunsetToSunrise => match sun? {
                crate::sun::Sun::Daily { sunrise, sunset } => (sunset, sunrise),
                crate::sun::Sun::NeverRises | crate::sun::Sun::NeverSets => return None,
            },
        };
        if from == until {
            return None;
        }
        match Self::within(from, until, minute) {
            true => Some(until),
            false => Some(from),
        }
    }

    /// The setting as the compositor is asked for it: what it should be doing
    /// now, and how warm.
    fn wanted_at(self, minute: u16, sun: Option<crate::sun::Sun>) -> (bool, u16) {
        (
            self.burning_at(minute, sun),
            self.temperature.clamp(WARMEST_KELVIN, NEUTRAL_KELVIN),
        )
    }
}

/// How many minutes a day has, which both ends of every window are counted in.
const MINUTES_IN_DAY: u16 = 24 * 60;

/// What one display turns out to be able to do, as reported by the compositor.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Support {
    /// This display can be driven in HDR.
    pub available: bool,
    /// It is being driven in it right now.
    pub active: bool,
    /// Its own peak, in cd/m². 0 when it does not say.
    pub peak: u16,
    /// Whether [`DisplayValue::SrgbIntensity`] does anything here. It is the
    /// CRTC's colour matrix, which needs a linear stage in front of it, and
    /// not every display engine has one.
    pub gamut: bool,
    /// This display's picture can be warmed: there is a gamma ramp behind it.
    ///
    /// A much shorter question than [`Self::available`], and asked separately
    /// for that reason — an ordinary SDR panel that will never do HDR can be
    /// warmed, so a Night light page built from the HDR list would leave the
    /// filter off exactly the displays that most want it.
    pub night_light: bool,
    /// Its picture is being warmed right now.
    pub warming: bool,
}

/// HDR settings by connector name, for every display the shell has been asked
/// about — including ones not currently plugged in, which is what lets a
/// display come back the way it was left.
static HDR: Mutex<BTreeMap<String, Hdr>> = Mutex::new(BTreeMap::new());

/// What each connected display reports, in the order the compositor announced
/// them, which is the order the screens are listed in.
static SUPPORT: Mutex<Vec<(String, Support)>> = Mutex::new(Vec::new());

/// What each connected display can be driven at, in the same order and for the
/// same reason. Kept apart from [`SUPPORT`] because it is a list rather than a
/// handful of flags, and because the two arrive in separate events.
static MODES: Mutex<Vec<(String, Vec<Offered>)>> = Mutex::new(Vec::new());

/// The mode chosen for a display, for the displays where either half of one
/// was: the two rows write to the same entry, because a connector is set to a
/// mode rather than to a size and a rate separately.
///
/// No inherited value stands behind this, unlike the HDR settings: a mode is a
/// statement about one connector's own list, and asking a display that has
/// never been configured for the size of the one beside it is asking for
/// something it may well not have. A display with no entry here is left at
/// whatever the compositor brought it up at.
static MODE: Mutex<BTreeMap<String, Mode>> = Mutex::new(BTreeMap::new());

/// How each display's picture is turned, for the displays the compositor turns
/// itself, in the order it announced them.
///
/// A display missing from here is one whose orientation is not the shell's to
/// set — a nested session, whose window is turned by the compositor above it —
/// which is why this is a list of what was reported rather than a value read
/// off every screen. It is the Orientation page's screen list, exactly as
/// [`SUPPORT`] is the HDR page's.
static TURNED: Mutex<Vec<(String, Orientation)>> = Mutex::new(Vec::new());

/// The orientation chosen for a display, for the displays one was chosen for.
///
/// Filed on its own, like [`MODE`] and for the same reason: a screen given a
/// turn has not thereby been given a colour pipeline. Nothing inherited stands
/// behind it either — a display nobody has turned is left the way the
/// compositor brought it up, which is its own config's answer and not the
/// shell's to overrule.
static TURN: Mutex<BTreeMap<String, Orientation>> = Mutex::new(BTreeMap::new());

/// Where each screen the compositor arranges stands in that arrangement,
/// counted from zero, in the order the compositor announced the screens.
///
/// Reported rather than remembered, as [`TURNED`] is, and the Display order
/// page's screen list for the same reason. A display missing from it is one
/// whose place is not the shell's to set — a nested session inside another
/// compositor, a screen the compositor's own config has pinned to a position,
/// or any screen at all on a session that mirrors them onto one region, where
/// there is no first screen to be.
///
/// In the announced order rather than in the order they are laid out, which is
/// the one thing here that is not the obvious choice. The page could list the
/// screens as the desk has them, and it would read well — but every press
/// would then reorder the rows under the cursor, and the cursor stays at the
/// row it was on. A user who moved their second screen to the front would find
/// themselves looking at a different screen's page with the mark apparently
/// unmoved, which is a press that reads as having failed. Announced order is
/// fixed for as long as the cables are, so the rows hold still and the mark
/// moves to where it was pressed. Every row says which place it holds, so
/// nothing about the arrangement is lost by not being able to read it off the
/// order of the list.
static PLACED: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());

/// The arrangement the user asked for: every display's place, counted from
/// zero, as of the last time anybody moved one.
///
/// The whole order rather than the one screen that was moved, because moving
/// one moves another — they trade — and half a permutation written down is an
/// order nobody asked for. Displays that have since been unplugged keep their
/// entries, which is what brings a screen back to its own place when it is
/// plugged in again.
///
/// Filed on its own, like [`MODE`] and [`TURN`], with nothing inherited behind
/// it: a screen nobody has moved is left where the compositor put it, which is
/// the order the displays were plugged in and not something the shell should
/// overrule by inventing one.
static PLACE: Mutex<BTreeMap<String, u32>> = Mutex::new(BTreeMap::new());

/// Each display's night light, for the displays one has been set on.
///
/// Filed on its own, like [`MODE`] and [`TURN`], and with nothing inherited
/// standing behind it — unlike the HDR settings, which have the flat keys an
/// older version of this file wrote. There is no older file to read here, and
/// a display nobody has warmed coming up unwarmed is never the wrong answer:
/// the setting's own default is to do nothing, so there is nothing for a
/// newly plugged screen to be missing.
static NIGHT: Mutex<BTreeMap<String, NightLight>> = Mutex::new(BTreeMap::new());

/// Which displays are to be rested while another one is being used, for the
/// displays somebody has answered the question on.
///
/// Filed on its own, like [`MODE`], [`TURN`] and [`NIGHT`], with nothing
/// inherited behind it and for the same reason: a screen nobody has asked to
/// rest is never rested, which is the setting's own default and is never the
/// wrong answer for a display this file has never heard of.
static OLED: Mutex<BTreeMap<String, bool>> = Mutex::new(BTreeMap::new());

/// What was read out of the settings file for displays it says nothing about.
///
/// The first version of this page had one set of HDR settings for the whole
/// session, written flat at the top of the file. A file written by that
/// version is still a statement of what the user wanted, so it becomes the
/// starting point for every display rather than being thrown away.
static INHERITED: Mutex<Hdr> = Mutex::new(Hdr {
    enabled: false,
    sdr_brightness: 200,
    srgb_intensity: 0,
    peak_brightness: 0,
});

/// What order each shelf of the user's own files is listed in, by the name the
/// row it hangs on has: `Music`, `Video`, `Images`.
///
/// Here rather than in [`crate::media`] for one reason: this is the module that
/// reads and writes the settings file, and everything in the file is built out
/// of the live values at the moment it is written — see [`stored`]. A setting
/// held somewhere the writer cannot see is a setting that gets left out of the
/// file the next time anything else changes.
///
/// Chosen from the Sort row of the context menu rather than from the Settings
/// column, which is the only reason it has no page of its own. It is a
/// preference all the same.
static MEDIA_SORT: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());

/// What the file explorer's key in that table is called.
///
/// The same table as the three shelves, because it is the same preference
/// chosen from the same row of the same menu — how somebody wants a folder
/// listed belongs beside how they want their music listed. One key rather than
/// one per folder: an order is how a person reads a list, not something they
/// hold about a particular directory, and a file kept per folder would be a
/// settings file that grew every time somebody looked in one.
const FILES: &str = "Files";

/// Write down that a shelf is listed in this order from now on.
pub fn remember_media_sort(kind: crate::media::Kind, sort: crate::media::Sort) {
    remember_sort(crate::apps::shelf_title(kind), sort);
}

/// The same for every folder of the file explorer.
pub fn remember_file_sort(sort: crate::media::Sort) {
    remember_sort(FILES, sort);
}

fn remember_sort(what: &str, sort: crate::media::Sort) {
    MEDIA_SORT
        .lock()
        .unwrap()
        .insert(what.to_string(), sort.key().to_string());
    save(&stored());
}

/// What the settings file says a shelf is listed in, if it says anything.
///
/// Asked once, when the library is made. An order the shell does not have —
/// a hand-edited typo, or a file written by a later version — is nothing
/// rather than an error: the shelf comes up alphabetical, which is the answer
/// that is never wrong.
pub fn media_sort(kind: crate::media::Kind) -> Option<crate::media::Sort> {
    sort_of(crate::apps::shelf_title(kind))
}

/// And what order every folder of the file explorer is listed in.
///
/// Asked once, when the shell starts, for the same reason: one order, held by
/// the shell, applied to every folder it reads.
pub fn file_sort() -> Option<crate::media::Sort> {
    sort_of(FILES)
}

fn sort_of(what: &str) -> Option<crate::media::Sort> {
    let held = MEDIA_SORT.lock().unwrap();
    let named = held.get(what)?;
    let sort = crate::media::Sort::from_key(named);
    if sort.is_none() {
        tracing::warn!(
            shelf = what,
            order = named,
            "the settings name an order this shell does not have"
        );
    }
    sort
}

/// Whether a folder is listed with the names that begin with a dot in it.
///
/// Off, which is what every file manager on the machine opens with and what
/// the explorer has always done: a home directory listed with them in it opens
/// on forty rows of program state before the first thing the user recognises,
/// and nobody's photographs are in `~/.cache`.
///
/// Here beside [`MEDIA_SORT`] because it is the same kind of preference chosen
/// from the same row of the same menu, and it is held the same way: one switch
/// for every folder rather than one per directory. Somebody who wants to see
/// what is in `~/.config` wants to see it in the folder under it too, and a
/// setting kept per folder would be a settings file that grew every time
/// anybody looked in one.
///
/// Written down for the reason an order is: nobody turns this on meaning
/// "until I next start the shell".
static SHOW_HIDDEN: Mutex<bool> = Mutex::new(false);

/// Whether this shell draws pictures of buttons to say what they do.
///
/// **On.** A console shell is the one kind of interface nobody arrives at
/// already knowing: there is no menu bar to read, no tooltip to hover, and the
/// buttons that do the work are on a pad whose letters differ between the three
/// companies that make them. A legend is how the shell says which one, and it
/// has to be there before anybody thinks to look for it — somebody who does not
/// need it is exactly the person who will find the switch, and somebody who
/// does will never go hunting for a setting to reveal what they do not know is
/// missing.
///
/// **One answer for the whole session**, which is the only shape this setting
/// can honestly have. It began as a rule about the start screen's corner, and
/// a shell that went on writing the same pictures a press away — in the menu,
/// on the friends list, at the foot of a file question — would be a switch that
/// did not do what its row says. Everything this shell draws a button with
/// reads it, and so does everything built on the toolkit: it is written to
/// `shell.toml` as `button-hints`, which is where an application that is not
/// this shell asks the same question. See `crate::ui::Legend`.
///
/// Session-wide beside the switches above it, and written down for the reason
/// they are: nobody turns this off meaning "until I next start the shell".
static BUTTON_HINTS: Mutex<bool> = Mutex::new(true);

/// Whether the shell says what its buttons do.
///
/// Read only. There is one row in the shell that changes it — Settings >
/// System > Button hints — and it goes through [`apply`] like every other
/// value on that column, which is also what writes it down. Nothing else in
/// the session has a reason to reach in, unlike [`show_hidden`] below, whose
/// second route is the Sort menu over a folder.
pub fn button_hints() -> bool {
    *BUTTON_HINTS.lock().unwrap()
}

/// Whether this shell drives Valve's client, or leaves Steam to be an
/// application like any other. See [`SteamValue::Integration`], where the
/// default is argued.
static STEAM_INTEGRATION: Mutex<bool> = Mutex::new(true);

/// Whether the client is started in the background as the session comes up.
/// See [`SteamValue::AtStartup`].
static STEAM_AT_STARTUP: Mutex<bool> = Mutex::new(false);

/// Whether the client is left running once a game has ended. See
/// [`SteamValue::AfterAGame`].
static STEAM_AFTER_A_GAME: Mutex<bool> = Mutex::new(true);

/// Whether this session does Steam at all, before the setting is even asked.
///
/// `--no-steam` is a session-wide refusal made on the command line, and it
/// outranks the file: a machine started that way must not be talked into a
/// worker by a settings file it happens to be carrying. Said once, by `main`,
/// before the first page is built — see [`note_steam_in_this_session`].
///
/// Its own answer rather than folded into [`STEAM_INTEGRATION`], because the
/// two say different things and the page has to be able to tell them apart: a
/// setting somebody turned off is a row to press again, and a flag on the
/// command line is a sentence explaining why there is no row.
static STEAM_IN_THIS_SESSION: Mutex<bool> = Mutex::new(true);

/// Whether Steam is this session's business at all.
///
/// Both halves at once, which is what every reader of this wants: the flag and
/// the setting are two ways of saying the same no, and nothing outside this
/// module has a reason to care which one was said.
pub fn steam_integration() -> bool {
    *STEAM_IN_THIS_SESSION.lock().unwrap() && *STEAM_INTEGRATION.lock().unwrap()
}

/// Whether Valve's client is started in the background as the shell comes up.
///
/// False in a session with no Steam in it, whichever of the two reasons it has
/// for that: there is no client to start on behalf of an integration that is
/// not running.
pub fn steam_at_startup() -> bool {
    steam_integration() && *STEAM_AT_STARTUP.lock().unwrap()
}

/// Whether Valve's client is left running once a game has ended.
///
/// True in a session with no Steam in it, and that is not the same answer worn
/// twice: a shell that is not driving the client has no business shutting one
/// down, so "leave it alone" is what an integration that is off must say.
pub fn steam_left_after_a_game() -> bool {
    !steam_integration() || *STEAM_AFTER_A_GAME.lock().unwrap()
}

/// Say whether this session was started with Steam left out.
///
/// Called once by `main`, from the command line, before the first settings
/// column is built.
pub fn note_steam_in_this_session(offered: bool) {
    *STEAM_IN_THIS_SESSION.lock().unwrap() = offered;
}

/// What the session's pointing devices do — see [`PointerValue`], which is
/// where each of the four is argued.
///
/// One answer for every mouse, touchpad and trackball at once. Held together
/// rather than as four statics because they are sent together: the compositor
/// applies three of them by walking the same device list, and four requests
/// would walk it four times for one press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pointer {
    /// libinput's acceleration in hundredths, -100 to 100.
    pub speed: i8,
    /// The cursor's size in logical pixels.
    pub size: u16,
    /// How far a wheel carries the content, in per cent.
    pub scroll: u16,
    /// Whether the content follows the fingers.
    pub natural: bool,
}

impl Pointer {
    /// What a session nobody has asked comes up with.
    ///
    /// Every one of these is somebody else's default rather than a number
    /// chosen here, and deliberately: a console that started with its own idea
    /// of how fast a mouse should be would be a machine that felt wrong to
    /// anybody who had ever used another one.
    ///
    /// `speed` is libinput's flat 0 — the middle of its range and what every
    /// desktop starts at. `size` is 24, which is XCursor's own default and what
    /// this compositor already used. `scroll` is one to one, the movement the
    /// device reported. `natural` is off, which is the wheel's traditional
    /// direction and what a mouse has always done.
    pub const DEFAULT: Self = Self {
        speed: 0,
        size: NATURAL_CURSOR,
        scroll: NATURAL_SCROLL,
        natural: false,
    };
}

impl Default for Pointer {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The cursor size a session nobody has asked comes up at, and the middle row
/// of [`CURSOR_SIZES`]. XCursor's own default, and every desktop's.
pub const NATURAL_CURSOR: u16 = 24;

/// One to one: a wheel carrying exactly what the device said it did.
pub const NATURAL_SCROLL: u16 = 100;

/// The sizes the cursor is offered at, smallest first, in logical pixels.
///
/// Four, not a bar, and the reason is what an XCursor theme *is*: it carries a
/// handful of drawn sizes and the nearest is used, so the values in between buy
/// a number that changes and a pointer that does not. These are the four sizes
/// every theme draws — and a size is chosen once, by looking at it, rather than
/// tuned.
pub const CURSOR_SIZES: [(u16, &str); 4] = [
    (16, "Small"),
    (NATURAL_CURSOR, "Normal"),
    (32, "Large"),
    (48, "Larger"),
];

/// The ends of the pointer speed bar, and how far one press moves it.
///
/// libinput's own range, in the hundredths the protocol carries. Ten steps
/// either side of the middle: fine enough that the right speed can be found and
/// coarse enough that finding it is a few presses rather than a walk.
pub const SLOWEST_POINTER: i8 = -100;
pub const FASTEST_POINTER: i8 = 100;
pub const POINTER_STEP: i8 = 10;

/// The same for the scrolling speed, in per cent of what the device reported.
///
/// A quarter of the movement at the foot and four times it at the head, in
/// steps of a quarter of natural. The foot is not nought — a wheel that goes
/// nowhere is a broken wheel, not a slow one — and the head is where one turn
/// is already most of a page.
pub const SLOWEST_SCROLL: u16 = 25;
pub const FASTEST_SCROLL: u16 = 400;
pub const SCROLL_STEP: u16 = 25;

/// The offered size nearest the one asked for.
///
/// A hand-edited file may name any number of pixels, and so may a shell from a
/// later version that offers a size this one does not. Neither is refused: the
/// nearest of [`CURSOR_SIZES`] is used, which is also what an XCursor theme
/// does with a size it has not drawn — so the answer the page shows and the
/// pointer on the screen agree, and the row is marked rather than the page
/// coming up with nothing chosen on it.
fn nearest_cursor_size(size: u16) -> u16 {
    CURSOR_SIZES
        .iter()
        .min_by_key(|(offered, _)| offered.abs_diff(size))
        .map(|(offered, _)| *offered)
        .unwrap_or(NATURAL_CURSOR)
}

/// What the pointing devices are set to.
static POINTER: Mutex<Pointer> = Mutex::new(Pointer::DEFAULT);

/// What every mouse on this machine does. Read by `main`, which sends it to the
/// compositor — this module records it and carries out none of it.
pub fn pointer() -> Pointer {
    *POINTER.lock().unwrap()
}

/// Which display the on-screen keyboard comes up on, by connector name, or
/// `None` for whichever screen the user is driving.
///
/// **`None`.** A board follows the hands: it is summoned from the pad or by a
/// text field taking the cursor, and both of those happen on the screen
/// somebody is looking at. Pinning it to a screen is the setting, and it is
/// there for the desk where one of the two displays is within reach — a
/// handheld panel beside a television, a touchscreen beside a monitor — and
/// where a keyboard on the other one is a keyboard nobody can type on.
///
/// **A name this session has no display for is left alone, not cleared.** The
/// board falls back to the driven screen for as long as that display is away
/// and goes back to it the moment it is plugged in again, which is what every
/// per-display setting in this shell does with a screen that comes and goes —
/// see [`StoredDisplay`]. A user who unplugs a monitor for the afternoon has
/// not changed their mind about where the keyboard belongs.
///
/// Held as a `String` rather than a `&'static str` for the reason
/// [`STARTUP_CATEGORY`] is: what is written down goes on being written down,
/// including a connector this session cannot see.
static KEYBOARD_DISPLAY: Mutex<Option<String>> = Mutex::new(None);

/// Which arrangement every keyboard on this machine is set to, as
/// `layout (variant)`, or `None` for a session nobody has asked.
///
/// **`None` is not "US".** It means the shell has no opinion, and what is in
/// force is then whatever the compositor's own `config.toml` says — which on a
/// machine whose owner set `keyboard_layout` there by hand is their answer, and
/// a shell that sent `us` at startup because it had never been asked would take
/// it away from them the first time this page shipped. The compositor reports
/// what it is using when the shell binds, and that is what the page marks until
/// somebody picks a row. See [`COMPOSITOR_LAYOUT`].
///
/// Held as a `String` for the reason [`KEYBOARD_DISPLAY`] is: what is written
/// down goes on being written down, including an arrangement this machine's
/// xkeyboard-config has no entry for.
static KEYBOARD_LAYOUT: Mutex<Option<String>> = Mutex::new(None);

/// The arrangement the compositor says it is using, which is the answer until
/// the shell has one of its own.
///
/// Reported over `lxb_shell_v1`, because the shell cannot read it: it is the
/// compositor's config file, and the compositor is the only process in the
/// session that has opened it. Without this the page could tick nothing on a
/// machine nobody had ever set a layout on, which is every machine the first
/// time it is opened.
static COMPOSITOR_LAYOUT: Mutex<Option<String>> = Mutex::new(None);

/// What is being typed into the field at the head of every column of the
/// keyboard layout tree.
///
/// One query for the whole tree and not one per column, which is what makes
/// the field mean the same thing wherever it is reached: it searches every
/// arrangement on the machine, so while there is something in it, whichever of
/// those columns is open shows what was found instead of what it is a list of.
/// A field that narrowed six continent names would be a row that did nothing.
/// See [`crate::apps::Searched::Layouts`].
///
/// Not written to the settings file. It is what somebody is typing, not
/// something they have set.
static LAYOUT_QUERY: Mutex<String> = Mutex::new(String::new());

/// Whether the compositor on the other end can be told a keyboard layout at
/// all.
///
/// A fact about the protocol version, like [`SCREEN_REST`], and answered once
/// for the session. Below it the page says so rather than offering six hundred
/// rows that change nothing — and the on-screen keyboard keeps its own ANSI
/// arrangement, because nothing will ever say what the keyboard is set to.
static KEYBOARD_LAYOUT_AVAILABLE: Mutex<bool> = Mutex::new(false);

/// Record whether the layout can be set. `true` when the column has to be
/// rebuilt to say so, as [`note_screen_rest`].
pub fn note_keyboard_layout_available(available: bool) -> bool {
    let mut held = KEYBOARD_LAYOUT_AVAILABLE.lock().unwrap();
    if *held == available {
        return false;
    }
    *held = available;
    true
}

/// Whether the page has anything behind it.
pub fn keyboard_layout_available() -> bool {
    *KEYBOARD_LAYOUT_AVAILABLE.lock().unwrap()
}

/// Let go of a layout the compositor would not compile.
///
/// The one path that un-sets this without the user asking, and it is not the
/// shell changing its mind: the compositor has said the arrangement does not
/// exist, so what is written down is a setting that can never come into force.
/// Left there it would be re-sent and refused at the start of every session.
pub fn forget_keyboard_layout() {
    *KEYBOARD_LAYOUT.lock().unwrap() = None;
    save(&stored());
}

/// The arrangement the shell has been told to use, if it has been told one.
pub fn keyboard_layout() -> Option<String> {
    KEYBOARD_LAYOUT.lock().unwrap().clone()
}

/// The arrangement actually in force: the shell's, or the compositor's own
/// where the shell has never been asked.
///
/// What the page ticks and what the row above it says. `None` only where the
/// shell has no setting *and* the compositor is too old to have reported one,
/// which is the one case where nothing in the session knows the answer.
pub fn keyboard_layout_in_force() -> Option<String> {
    keyboard_layout().or_else(|| COMPOSITOR_LAYOUT.lock().unwrap().clone())
}

/// Record what the compositor says its keyboard is set to. `true` when the
/// column has to be rebuilt to say so, as [`note_screen_rest`].
pub fn note_compositor_layout(key: String) -> bool {
    let mut held = COMPOSITOR_LAYOUT.lock().unwrap();
    if held.as_deref() == Some(key.as_str()) {
        return false;
    }
    *held = Some(key);
    // Only where the shell has none of its own: the page is showing the
    // shell's answer, and a compositor that reported the layout it was just
    // *told* to use would not have changed anything on screen.
    KEYBOARD_LAYOUT.lock().unwrap().is_none()
}

/// What is in the layout field at the moment.
pub fn layout_query() -> String {
    LAYOUT_QUERY.lock().unwrap().clone()
}

/// Put something in that field, or empty it. `true` when the column has to be
/// rebuilt, which is whenever it changed.
pub fn set_layout_query(query: &str) -> bool {
    let mut held = LAYOUT_QUERY.lock().unwrap();
    if *held == query {
        return false;
    }
    *held = query.to_string();
    true
}

/// The display the on-screen keyboard has been pinned to, if it has been
/// pinned to one. Not checked against the screens this session has — that is
/// the caller's, because only the caller knows which screens it is drawing on.
pub fn keyboard_display() -> Option<String> {
    KEYBOARD_DISPLAY.lock().unwrap().clone()
}

/// The column the start screen opens on.
///
/// **Games.** A console is a machine for playing things, and the column a
/// person wants is the one holding what they came to the machine to do. The
/// shell opened on the first column with anything in it before this setting
/// existed, which on nearly every machine is System — the disk and the
/// administrative tools, which is where somebody goes when something is wrong
/// rather than when it is right.
///
/// The id and not the column, because the bar is not built yet when this is
/// read from the file, and because a column that is not there this session
/// still has to go on being the setting: a machine whose Steam account has been
/// signed out has no Steam column, and a shell that quietly rewrote the setting
/// to something else would lose the answer for good. What happens when the
/// named column is not on the bar is [`crate::model::Cursor::for_model`]'s
/// business, and it is what the shell did before this existed — the first
/// column with something in it.
///
/// Held as a `String` rather than a `&'static str` for the reason
/// [`STEAM_SORT`] is: a file naming a column this shell does not have keeps
/// naming it, so that a session which read a later version's setting, changed
/// the accent and wrote the file back does not silently throw the user's choice
/// away.
static STARTUP_CATEGORY: Mutex<Option<String>> = Mutex::new(None);

/// What the start screen opens on.
pub fn startup_category() -> String {
    STARTUP_CATEGORY
        .lock()
        .unwrap()
        .clone()
        .unwrap_or_else(|| DEFAULT_STARTUP_CATEGORY.to_string())
}

/// Where a shell nobody has told opens. See [`STARTUP_CATEGORY`].
const DEFAULT_STARTUP_CATEGORY: &str = "games";

/// Set it without writing anything down.
///
/// For the tests, and reached from [`crate::model`]'s as well as this module's:
/// what the setting *does* is decide where a cursor is built standing, and that
/// is `model`'s to assert. `None` puts it back to having never been asked.
/// Never [`apply`], which writes to the config directory of whoever is running
/// the suite.
#[cfg(test)]
pub fn note_startup_category(id: Option<&str>) {
    *STARTUP_CATEGORY.lock().unwrap() = id.map(str::to_string);
}

/// Whether the explorer is listing the names that begin with a dot.
pub fn show_hidden() -> bool {
    *SHOW_HIDDEN.lock().unwrap()
}

/// Turn it over, and write it down. Reports where it ended up, which is what
/// the tick on the menu row draws.
pub fn set_show_hidden(on: bool) -> bool {
    {
        let mut held = SHOW_HIDDEN.lock().unwrap();
        if *held == on {
            return on;
        }
        *held = on;
    }
    tracing::info!(on, "listing the hidden names");
    save(&stored());
    on
}

/// What order the Steam column is listed in, by the name the order goes under
/// in the file.
///
/// One order rather than a map like [`MEDIA_SORT`], because there is one Steam
/// column: a person has one library, and the question "which of these do I want
/// to see first" is asked of the whole of it. Held here for the reason the
/// shelves' orders are — the file is built out of the live values whenever
/// anything is written, so a setting the writer cannot see is one the next
/// change to anything else drops.
///
/// The key rather than the order itself, so that a file naming an order this
/// shell does not have keeps naming it: a session that read a later version's
/// setting, changed the accent and wrote the file back would otherwise silently
/// throw the user's choice away.
static STEAM_SORT: Mutex<Option<String>> = Mutex::new(None);

/// Write down that the Steam column is listed in this order from now on.
pub fn remember_steam_sort(sort: lxb_steam::library::Sort) {
    *STEAM_SORT.lock().unwrap() = Some(sort.key().to_string());
    save(&stored());
}

/// What the settings file says the Steam column is listed in, if it says
/// anything.
///
/// Asked once, when the session's Steam is started. An order the shell does not
/// have is nothing rather than an error — the column comes up installed-first,
/// which is the answer it has always come up in.
pub fn steam_sort() -> Option<lxb_steam::library::Sort> {
    let held = STEAM_SORT.lock().unwrap();
    let named = held.as_deref()?;
    let sort = lxb_steam::library::Sort::from_key(named);
    if sort.is_none() {
        tracing::warn!(
            order = named,
            "the settings name a Steam order this shell does not have"
        );
    }
    sort
}

/// How loud the shell's own effects and Start music are, and whether they are
/// silenced.
///
/// The shell's own, and nothing else's. The volume bar in the guide's sidebar
/// sets what the whole session comes out at — every application on the machine
/// with it — and that bar is there whether or not the mixer beside it opens.
/// This is the other thing a console has: how loudly the interface answers and
/// its own background plays, which is a preference about the shell rather than
/// about the machine, and which is why the mixer's own System row sets this and
/// not that.
///
/// Held here for the reason [`MEDIA_SORT`] is: everything in the settings file
/// is built out of the live values at the moment it is written, and a value
/// the writer cannot see is one that gets dropped the next time anything else
/// changes.
static SOUND: Mutex<Level> = Mutex::new(Level {
    value: 1.0,
    muted: false,
});

/// Where the shell's effects and Start music stand.
pub fn sound() -> Level {
    *SOUND.lock().unwrap()
}

/// Whether the Start screen plays its background music at all.
///
/// Separate from [`SOUND`], and deliberately: that is *how loud* the shell is,
/// and it is one answer for the whole of it — silencing the music with it would
/// silence every click as well. This is the other question a console asks, which
/// is whether the screen with nothing open on it plays anything, and it is
/// answered without touching what the buttons sound like.
///
/// Kept here for the reason [`MEDIA_SORT`] and [`SOUND`] are: the settings file
/// is built out of the live values at the moment it is written, so a value the
/// writer cannot see is one the next change to anything else drops.
static START_MUSIC: Mutex<bool> = Mutex::new(true);

/// The file standing behind everything, where the wallpaper is one of the
/// user's own rather than the shell's scene.
///
/// The shell's own copy of it — see [`crate::paper::keep`] — because that is
/// what the setting has to name if it is to mean anything a week later: the file
/// the user pressed may be on a stick, in a folder they are about to tidy, or in
/// a download they are about to clear out.
///
/// Kept whatever the Theme setting says, and deliberately. Somebody who stands
/// the wallpaper down to Simple for an evening's game has not thrown their
/// picture away, and the shell that comes up in Custom tomorrow reads this to
/// know what to draw. It is the *style* that says whether it is on screen.
static CUSTOM_WALLPAPER: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Where somebody keeps their ROMs, if they have said.
///
/// The folder itself and not what is in it: what consoles are in there is a
/// question for the helper that reads the disk, asked again whenever this
/// changes — see [`crate::retroarch`]. A shell that wrote down the answer as
/// well would come up tomorrow listing a game that was deleted last night.
///
/// Kept whether or not RetroArch is still installed, on the terms the
/// wallpaper's file is kept: somebody who removes RetroArch for a week has not
/// re-sorted their collection, and being asked for the folder again afterwards
/// would be the shell forgetting something it was told.
static ROMS_FOLDER: Mutex<Option<PathBuf>> = Mutex::new(None);

/// The folder somebody's games are in, if they have chosen one.
pub fn roms_folder() -> Option<PathBuf> {
    ROMS_FOLDER.lock().unwrap().clone()
}

/// Their games are in there from now on.
///
/// Written the moment it is chosen rather than when the folder has been read:
/// the user answered a question and the answer is theirs whatever the scan
/// finds in it, including nothing.
pub fn choose_roms_folder(at: &Path) {
    *ROMS_FOLDER.lock().unwrap() = Some(at.to_path_buf());
    save(&stored());
    tracing::info!(at = %at.display(), "the ROM folder");
}

/// Forget where they are, because the thing that read it has gone.
///
/// The only setting in this file that belongs to the RetroArch integration and
/// to nothing else, so it goes when RetroArch does — see
/// `Shell::retroarch_removed`. Their games are not touched and the folder is
/// not touched; what is forgotten is one line saying where to look.
pub fn forget_roms_folder() {
    *ROMS_FOLDER.lock().unwrap() = None;
    save(&stored());
    tracing::info!("the ROM folder is forgotten");
}

/// What a column of folders is being walked *for*.
///
/// One answer today and it is an enum anyway, for the reason [`Setting`] is
/// one: the picker is a walk over the disk that ends in a press, and what that
/// press means has to travel with the walk — see [`crate::files::Shows`],
/// which is what carries it down each step. A second thing to choose a folder
/// for is a variant here and an arm in `Shell::picked`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picking {
    /// Where somebody's ROMs are, for Settings > Games > RetroArch and for the
    /// setup the RetroArch row asks for the first time it is pressed.
    RomsFolder,
    /// Where somebody's BIOS dumps are, for the console that cannot start
    /// without one. What is chosen is copied — see `Shell::take_the_firmware`.
    Firmware,
}

impl Picking {
    /// The line under the row that answers the picker, which is what pressing
    /// it would mean.
    pub fn note(self) -> &'static str {
        match self {
            Picking::RomsFolder => "Look for games in this folder",
            Picking::Firmware => "Copy the BIOS out of this folder",
        }
    }
}

/// The picture or film the wallpaper is set to, if the user has chosen one.
///
/// Says nothing about whether it is being drawn: that is
/// `theme::applied_style(theme::Part::Wallpaper)`, and the two are separate
/// answers on purpose.
pub fn custom_wallpaper() -> Option<PathBuf> {
    CUSTOM_WALLPAPER.lock().unwrap().clone()
}

/// Draw this picture or film behind everything from now on.
///
/// Both halves of the answer at once, because they have to be one answer: the
/// setting points at the file, and the Theme setting is turned to Custom. A
/// shell that had done one of the two would come up next time drawing a scene
/// with a photograph ticked in its Settings column, or the other way about.
///
/// The file the user pressed, and only for as long as it takes to copy it: the
/// copy is made on a thread — see [`crate::paper::Paper::keep_later`] — and
/// [`note_custom_wallpaper`] points this at it when it lands. Which means a
/// session that ends in between names a file that is still on the disk and comes
/// back up drawing it, and that is the honest state to be caught in.
pub fn choose_custom_wallpaper(source: &Path) {
    *CUSTOM_WALLPAPER.lock().unwrap() = Some(source.to_path_buf());
    theme::commit_style(theme::Part::Wallpaper, wallpaper::CUSTOM);
    save(&stored());
    tracing::info!(file = %source.display(), "custom wallpaper");
}

/// The copy has landed: from now on it is the file the setting names.
///
/// Nothing else changes and nothing is redrawn. It is the same picture under
/// another name — what it buys is the next session, and every session after the
/// user has moved or deleted what they chose.
pub fn note_custom_wallpaper(kept: PathBuf) {
    let mut file = CUSTOM_WALLPAPER.lock().unwrap();
    if file.as_ref() == Some(&kept) {
        return;
    }
    *file = Some(kept);
    drop(file);
    save(&stored());
}

/// Stop claiming to have a wallpaper of the user's own.
///
/// One caller: a file that turns out not to be one this shell can draw, pressed
/// a moment ago. It puts the material back to the shell's own and forgets the
/// file, which is what the column has to show — the alternative is a row ticked
/// for a picture nobody can see.
///
/// Deliberately not what choosing Default or Simple does. That is somebody
/// saying which of the three they want on screen, and their picture is still
/// their picture.
pub fn forget_custom_wallpaper() {
    *CUSTOM_WALLPAPER.lock().unwrap() = None;
    theme::commit_style(theme::Part::Wallpaper, wallpaper::STYLES[0]);
    save(&stored());
}

/// Whether the Start screen's background music plays.
///
/// On, until somebody says otherwise: it is what the shell has always come up
/// doing, and a console that arrived silent would leave the user looking for the
/// row that turned it off.
pub fn start_music() -> bool {
    *START_MUSIC.lock().unwrap()
}

/// Whether the battery's charge is written out in figures in the corner.
///
/// Off, unlike the music above it, and for the reason that one is on: what a
/// console does by default is the thing somebody would not have to go looking
/// for. Music is what the Start screen has always done. A number on the
/// wallpaper is not — the mark says how much is left, which is what a glance
/// at a corner asks, and the figures are for somebody who came looking for the
/// exact charge and will find the row when they do.
///
/// Kept here for the reason [`START_MUSIC`] and [`SOUND`] are: the settings
/// file is built out of the live values at the moment it is written, so a value
/// the writer cannot see is one the next change to anything else drops.
static BATTERY_PERCENT: Mutex<bool> = Mutex::new(false);

/// Whether the battery's charge is written out in figures beside the clock.
pub fn battery_percent() -> bool {
    *BATTERY_PERCENT.lock().unwrap()
}

/// What is in this machine's battery, as [`crate::power`] last found it, and
/// `None` for a machine that has none.
///
/// Reported rather than remembered, on exactly the terms [`DEVICES`] and
/// [`NETWORK`] are: it is a statement about hardware made by something outside
/// this module, and nothing of it goes into the settings file. A laptop whose
/// battery has been taken out is a machine with none, and both the row and the
/// switch under it go away with it — the alternative is a settings page
/// describing a shell that this machine cannot show.
static BATTERY: Mutex<Option<crate::power::Charge>> = Mutex::new(None);

/// Record what the battery said. `true` when the column has to be rebuilt to
/// say so — as [`note_devices`].
///
/// Which is *not* every change, unlike the two above it, and this is the one
/// place that distinction is worth drawing: a discharging battery reports a
/// different number every time it is read, and rebuilding the Settings column
/// three times a minute for the length of a session would be a page rebuilt for
/// a difference it does not show. What the column shows is the row's drawing,
/// so that is what is compared.
pub fn note_battery(charge: Option<crate::power::Charge>) -> bool {
    let mut held = BATTERY.lock().unwrap();
    let shown = |charge: &Option<crate::power::Charge>| charge.map(crate::ui::battery_glyph);
    let changed = shown(&held) != shown(&charge);
    *held = charge;
    changed
}

/// Whether this session's compositor can rest a display behind black at all.
///
/// Reported rather than remembered, on the terms [`BATTERY`] is: it is a
/// statement about what is on the other end of `lxb_shell_v1`, made by the
/// half of the shell that holds the connection. Nothing of it goes into the
/// settings file — the setting itself is written down as usual, because a
/// session moved to a newer compositor should find its screens still set the
/// way they were left.
///
/// It is one answer for the whole session and not one per display, unlike
/// everything else under Display, because it is a fact about the protocol
/// rather than about a screen: a compositor either draws these sheets or it
/// does not, and it draws them over any display it is driving.
static SCREEN_REST: Mutex<bool> = Mutex::new(false);

/// Record whether screens can be rested. `true` when the column has to be
/// rebuilt to say so, as [`note_support`].
pub fn note_screen_rest(available: bool) -> bool {
    let mut held = SCREEN_REST.lock().unwrap();
    if *held == available {
        return false;
    }
    *held = available;
    true
}

/// Whether the page has anything behind it.
pub fn screen_rest_available() -> bool {
    *SCREEN_REST.lock().unwrap()
}

/// How large every application draws its own interface, in per cent of the size
/// it chose. See [`application_scale`].
///
/// Session-wide, and the one setting here that is neither the shell's own
/// appearance nor a property of a screen: it is carried out by the compositor,
/// for every application on the machine, and what it answers is how far away the
/// user is sitting.
///
/// Kept here for the reason [`START_MUSIC`] and [`SOUND`] are: the settings file
/// is built out of the live values at the moment it is written, so a value the
/// writer cannot see is one the next change to anything else drops.
static APP_SCALE: Mutex<u16> = Mutex::new(NATURAL_SCALE);

/// How large applications are being drawn, in per cent. 100 is one to one.
///
/// One to one until somebody says otherwise, which is the only defensible
/// default: a shell that came up magnifying every window would look like one
/// that could not read its own display's size.
pub fn app_scale() -> u16 {
    *APP_SCALE.lock().unwrap()
}

/// What a browser's picture-in-picture window is given: whether it floats at
/// all, how large it is drawn and which corner it sits in.
///
/// Kept here for the reason [`APP_SCALE`] is: the settings file is built out of
/// the live values at the moment it is written, so a value the writer cannot see
/// is one the next change to anything else drops. Carried out by the compositor
/// — `main` sends it over `lxb_shell_v1` — and remembered by nothing else.
static PICTURE_IN_PICTURE: Mutex<Pip> = Mutex::new(Pip::DEFAULT);

/// What the floating window is set to.
///
/// The default is the feature switched on, at a quarter of the display's width,
/// in the upper right corner. On, because a shell that came up ignoring the
/// button a browser offers would look like one whose picture-in-picture is
/// broken; upper right, because that is the corner the feature is named after
/// everywhere it exists.
pub fn picture_in_picture() -> Pip {
    *PICTURE_IN_PICTURE.lock().unwrap()
}

/// What the shell remembers about the floating window.
///
/// The three halves of one page, in one value rather than three, because they
/// are sent to the compositor in one request: a size without its corner would
/// move the window twice for one press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pip {
    pub floating: bool,
    pub size: pip::Size,
    pub place: pip::Place,
}

impl Pip {
    /// What a machine nobody has set this on comes up with. A `const` rather
    /// than a `Default`, because a `Mutex` in a static has to be built in one.
    const DEFAULT: Self = Self {
        floating: true,
        size: pip::Size::Medium,
        place: pip::Place::TopRight,
    };
}

impl Default for Pip {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Whether anything is allowed to interrupt: the guide's do-not-disturb tile.
///
/// Here rather than in [`crate::pointer::Prefs`], which is the other file the
/// guide writes, because that one is keyed by application and this is a
/// statement about the session — a user who does not want to be interrupted
/// does not want it in the browser either.
///
/// Remembered across a session for the same reason the Start music is: it is a
/// switch somebody threw on purpose, and a console that had quietly turned it
/// back off overnight would deliver a night's announcements at breakfast.
static DO_NOT_DISTURB: Mutex<bool> = Mutex::new(false);

/// Whether announcements are being kept out of the corner of the screen.
///
/// Off until somebody says otherwise: a shell that came up refusing to show
/// what the machine had to say would look like one whose notifications are
/// broken.
pub fn do_not_disturb() -> bool {
    *DO_NOT_DISTURB.lock().unwrap()
}

/// Turn it over, and write it down. Reports where it ended up, which is what
/// the tile draws and what the notification centre is told.
pub fn set_do_not_disturb(on: bool) -> bool {
    {
        let mut held = DO_NOT_DISTURB.lock().unwrap();
        if *held == on {
            return on;
        }
        *held = on;
    }
    tracing::info!(on, "do not disturb");
    save(&stored());
    on
}

/// Which control the user has in their hands: the controller, or a keyboard.
///
/// Not a setting anybody chooses from a row — there is no page for it, and
/// there should not be. It is an observation, made from the two things the
/// shell can watch: a button pressed or a stick pushed on a pad it reads
/// straight from `/dev/input`, and a key pressed on a keyboard, which reaches
/// it either through its own focus or through the compositor's `typed` event
/// while an application holds the keys.
///
/// What it decides is what the shell offers a controller. The corner chip that
/// names the two buttons for the on-screen keyboard is a reminder for somebody
/// holding a pad; over the shoulder of somebody typing it is a picture of the
/// letters already under their hands, sitting on top of the thing they are
/// typing into. The keyboard the shell would raise over a text field by itself
/// is the same offer, larger.
///
/// Written down for the reason the do-not-disturb switch is, and with less
/// excuse for getting it wrong: it is a statement about a person rather than
/// about a session, and somebody who spent all of last night typing does not
/// become a controller user again by turning the machine off. A console that
/// forgot would throw a keyboard over the first text field of every morning.
///
/// True until something says otherwise. A console with nobody's habits recorded
/// yet is a console, and the pad is what it is held with.
static CONTROLLER_IN_HAND: Mutex<bool> = Mutex::new(true);

/// Whether the controller is what the user last reached for.
pub fn controller_in_hand() -> bool {
    *CONTROLLER_IN_HAND.lock().unwrap()
}

/// Record which control is in hand now. Reports whether it is a change, which
/// is when the shell has anything to do about it — the corner chip to put away
/// or bring back, and the compositor to tell.
pub fn set_controller_in_hand(in_hand: bool) -> bool {
    {
        let mut held = CONTROLLER_IN_HAND.lock().unwrap();
        if *held == in_hand {
            return false;
        }
        *held = in_hand;
    }
    tracing::info!(in_hand, "the controller is what is in hand");
    save(&stored());
    true
}

/// What the machine can play through and record from, as the sound server last
/// listed them, and which of them it is using.
///
/// Reported rather than remembered, exactly as [`SUPPORT`] and [`MODES`] are:
/// it is a statement about hardware that is plugged in at this moment, made by
/// something outside this module — [`crate::system`] there, the compositor
/// here — and the page is rebuilt when it changes. Nothing of it goes into the
/// settings file. See [`Setting::SoundDevice`].
static DEVICES: Mutex<Devices> = Mutex::new(Devices::none());

/// Record what the sound server said. `true` when it is a change, and so when
/// the column has to be rebuilt to say so — as [`note_support`].
pub fn note_devices(reported: Devices) -> bool {
    let mut held = DEVICES.lock().unwrap();
    if *held == reported {
        return false;
    }
    *held = reported;
    true
}

/// What this machine is on, as [`crate::network`] last found it.
///
/// Reported rather than remembered, on exactly the terms [`DEVICES`] is: it
/// describes hardware that is plugged in and air that is being listened to at
/// this moment, it is read by something outside this module, and nothing of it
/// goes into the settings file. See [`Setting::Network`].
static NETWORK: Mutex<crate::network::Listing> = Mutex::new(crate::network::Listing::none());

/// What Steam last said about which compatibility tool runs everything it has
/// not verified.
///
/// Reported rather than remembered, on exactly the terms [`DEVICES`] and
/// [`NETWORK`] are, and for one reason more than they have: this setting is
/// not this shell's. It lives in Valve's own configuration, the client's own
/// settings page sets the same thing, and a copy kept in the settings file
/// would be a second opinion that goes stale the first time somebody changes
/// it in Steam. See [`SteamValue::OtherTitles`].
static COMPATIBILITY: Mutex<Option<crate::steam::Compat>> = Mutex::new(None);

/// Record what Steam said about it. `true` when it is a change, and so when the
/// column has to be rebuilt to say so — as [`note_devices`].
pub fn note_compatibility_tools(said: Option<&crate::steam::Compat>) -> bool {
    let mut held = COMPATIBILITY.lock().unwrap();
    if held.as_ref() == said {
        return false;
    }
    *held = said.cloned();
    true
}

/// Record what the network manager said. `true` when it is a change, and so
/// when the column has to be rebuilt to say so — as [`note_devices`].
pub fn note_network(reported: crate::network::Listing) -> bool {
    let mut held = NETWORK.lock().unwrap();
    if *held == reported {
        return false;
    }
    *held = reported;
    true
}

/// What the Network pages are drawn from.
pub fn network_listing() -> crate::network::Listing {
    NETWORK.lock().unwrap().clone()
}

/// What this machine is paired with, as [`crate::bluetooth`] last found it.
///
/// Reported rather than remembered, on the terms [`NETWORK`] is and for the
/// same reason: it describes a controller that is switched on and air that is
/// being listened to at this moment, and nothing of it goes into the settings
/// file. See [`Setting::Bluetooth`].
static BLUETOOTH: Mutex<crate::bluetooth::Listing> = Mutex::new(crate::bluetooth::Listing::none());

/// Record what BlueZ said. `true` when it is a change, and so when the column
/// has to be rebuilt to say so — as [`note_network`].
pub fn note_bluetooth(reported: crate::bluetooth::Listing) -> bool {
    let mut held = BLUETOOTH.lock().unwrap();
    if *held == reported {
        return false;
    }
    *held = reported;
    true
}

/// What the Bluetooth pages are drawn from.
pub fn bluetooth_listing() -> crate::bluetooth::Listing {
    BLUETOOTH.lock().unwrap().clone()
}

/// Which controller the machine's Bluetooth *is*, by its address.
///
/// Three things about Bluetooth are the shell's own and are written down, and
/// this is the first: a machine with a card on the board and a dongle in the
/// front has two radios and one of them is the one the user means. BlueZ has no
/// opinion about that — every adapter is equal to it — so somebody has to keep
/// the answer, and it has to survive a reboot.
///
/// The **address** rather than `hci0`, because the numbering is the order the
/// kernel happened to probe them in: a dongle plugged in before the machine
/// booted is `hci0` today and `hci1` tomorrow, and a preference pinned to that
/// would quietly move to the other radio. An address does not move.
///
/// `None` until somebody chooses, which is a machine that uses the first
/// controller BlueZ lists — see [`chosen_controller`].
static BLUETOOTH_CONTROLLER: Mutex<Option<String>> = Mutex::new(None);

pub fn bluetooth_controller() -> Option<String> {
    BLUETOOTH_CONTROLLER.lock().unwrap().clone()
}

/// What happens to Bluetooth when a session starts. The second of the three
/// answers BlueZ does not have.
static BLUETOOTH_STARTUP: Mutex<Startup> = Mutex::new(Startup::Restore);

pub fn bluetooth_startup() -> Startup {
    *BLUETOOTH_STARTUP.lock().unwrap()
}

/// Whether Bluetooth was on when this shell last looked. The third, and the one
/// nothing chooses.
///
/// Written by the shell watching rather than by anybody pressing anything,
/// which makes it the counterpart of [`CONTROLLER_IN_HAND`]: it is a fact about
/// the last session that the next one needs, and there is no row for it. It is
/// what [`Startup::Restore`] restores.
static BLUETOOTH_WAS_ON: Mutex<bool> = Mutex::new(true);

pub fn bluetooth_was_on() -> bool {
    *BLUETOOTH_WAS_ON.lock().unwrap()
}

/// Record whether Bluetooth is on, writing the file only when it has moved.
///
/// Called from the frame that notices a change rather than on the way out,
/// because there is no way out to rely on: a session ends by the machine being
/// switched off at least as often as by anybody signing out, and a value only
/// written at shutdown is one that is not there after the times it matters
/// most.
pub fn note_bluetooth_powered(on: bool) {
    if remember_bluetooth_powered(on) {
        save(&stored());
    }
}

/// Hold whether Bluetooth is on, and say whether that is news.
///
/// Split from the write for the reason the volume's pair is split — see
/// [`the_shell_s_own_volume_is_remembered`]: a test that went through the
/// function above would write to the config directory of whoever is running the
/// tests, and this one did, for as long as it took somebody to notice their own
/// shelf orders changing when they ran `cargo test`.
///
/// [`the_shell_s_own_volume_is_remembered`]: tests::the_shell_s_own_volume_is_remembered
fn remember_bluetooth_powered(on: bool) -> bool {
    let mut held = BLUETOOTH_WAS_ON.lock().unwrap();
    if *held == on {
        return false;
    }
    *held = on;
    true
}

/// Set them, and write it down. Reports whether anything moved.
///
/// Written on every step rather than when the user stops moving the row. A
/// shell that waited would have to be woken to do the writing, and an idle bar
/// blocks until something happens to it — so the level a user set and then
/// walked away from would be the one level in the file that a machine switched
/// off at the wall could lose. The file is small and replaced through a
/// rename; a held direction is a second or two of that.
pub fn set_sound(level: Level) -> bool {
    let level = Level {
        value: level.value.clamp(0.0, 1.0),
        muted: level.muted,
    };
    {
        let mut held = SOUND.lock().unwrap();
        if *held == level {
            return false;
        }
        *held = level;
    }
    save(&stored());
    true
}

/// What the shell is asking the compositor for on one display.
pub fn hdr_for(display: &str) -> Hdr {
    HDR.lock()
        .unwrap()
        .get(display)
        .copied()
        .unwrap_or_else(|| *INHERITED.lock().unwrap())
}

/// What every display reports, newest picture first announced first.
pub fn support() -> Vec<(String, Support)> {
    SUPPORT.lock().unwrap().clone()
}

/// Record what the compositor said about the displays. `true` when it is a
/// change, and so when the column has to be rebuilt to say so.
pub fn note_support(reported: Vec<(String, Support)>) -> bool {
    let mut held = SUPPORT.lock().unwrap();
    if *held == reported {
        return false;
    }
    *held = reported;
    true
}

/// What every display can be driven at, in the order they were announced.
pub fn modes() -> Vec<(String, Vec<Offered>)> {
    MODES.lock().unwrap().clone()
}

/// Record the mode lists. `true` when it is a change, as [`note_support`].
pub fn note_modes(reported: Vec<(String, Vec<Offered>)>) -> bool {
    let mut held = MODES.lock().unwrap();
    if *held == reported {
        return false;
    }
    *held = reported;
    true
}

/// The mode the shell is asking one display for, if it has ever been asked for
/// one.
pub fn mode_for(display: &str) -> Option<Mode> {
    MODE.lock().unwrap().get(display).copied()
}

/// How every display the compositor turns itself is currently turned, in the
/// order they were announced.
pub fn turned() -> Vec<(String, Orientation)> {
    TURNED.lock().unwrap().clone()
}

/// Record what the compositor said about the orientations. `true` when it is a
/// change, as [`note_support`].
pub fn note_turned(reported: Vec<(String, Orientation)>) -> bool {
    let mut held = TURNED.lock().unwrap();
    if *held == reported {
        return false;
    }
    *held = reported;
    true
}

/// The orientation the shell is asking one display for, if it has ever been
/// asked for one.
pub fn turn_for(display: &str) -> Option<Orientation> {
    TURN.lock().unwrap().get(display).copied()
}

/// Where each screen the compositor arranges stands, in the order the screens
/// were announced — which is the order the page lists them in.
pub fn placed() -> Vec<(String, u32)> {
    PLACED.lock().unwrap().clone()
}

/// Record where the compositor says the displays stand. `true` when it is a
/// change, as [`note_support`].
pub fn note_places(reported: Vec<(String, u32)>) -> bool {
    let mut held = PLACED.lock().unwrap();
    if *held == reported {
        return false;
    }
    *held = reported;
    true
}

/// The same screens read the other way round: the arrangement itself, first
/// screen first.
///
/// The places reported are one list's own indices, so this is a sort by them.
/// Ties are broken by name, which cannot arise from a compositor that follows
/// the protocol and is here so that one which does not still gives the page a
/// fixed order rather than one that changes from frame to frame.
fn arrangement() -> Vec<String> {
    let mut order = placed();
    order.sort_by(|(left, left_place), (right, right_place)| {
        left_place.cmp(right_place).then_with(|| left.cmp(right))
    });
    order.into_iter().map(|(name, _)| name).collect()
}

/// The arrangement the shell is asking for, out of what it has been told is in
/// force and what it remembers being asked for.
///
/// Names in order, first screen first, and only screens the compositor says it
/// arranges — asking for a display that takes no part in the layout would be
/// asking for nothing.
///
/// A screen the file has never heard of keeps its place at the back of the
/// ones it has, which is where the compositor itself puts an arriving display:
/// the sort is stable, so anything with no remembered place holds the place it
/// was reported at. That is what makes plugging in a new monitor leave the
/// arrangement alone instead of shuffling it.
pub fn wanted_order() -> Vec<String> {
    let held = PLACE.lock().unwrap();
    let mut order = arrangement();
    order.sort_by_key(|name| held.get(name).copied().unwrap_or(u32::MAX));
    order
}

/// What one display's night light is set to. A display nobody has set one on
/// is not warmed, which is what [`NightLight::default`] says.
pub fn night_light_for(display: &str) -> NightLight {
    NIGHT
        .lock()
        .unwrap()
        .get(display)
        .copied()
        .unwrap_or_default()
}

/// Whether one display is to be rested while another one is being used. A
/// display nobody has answered for is left alone, which is what a page that
/// has never been visited should do.
pub fn oled_protection_for(display: &str) -> bool {
    OLED.lock().unwrap().get(display).copied().unwrap_or(false)
}

/// What the compositor should be asked for on one display *now*: whether the
/// light should be burning at this moment, and how warm.
///
/// The schedule is resolved here rather than sent, because a schedule is a
/// clock and a time zone and the compositor has no business owning either —
/// see the `set_output_night_light` request, which takes only the answer.
///
/// A session whose local time cannot be read at all — which would be a C
/// library that has lost `/etc/localtime` — is treated as having no schedule
/// rather than as having an unsatisfied one: the switch then means what it
/// says, which is far better than a light that never comes on and a page that
/// cannot explain why.
pub fn night_light_now(display: &str) -> (bool, u16) {
    let setting = night_light_for(display);
    match local_time() {
        Some(now) => setting.wanted_at(now.minute_of_day(), sun_today()),
        None => NightLight {
            schedule: Schedule::AllDay,
            ..setting
        }
        .wanted_at(0, None),
    }
}

/// The modes one display offers, as the compositor last listed them.
fn offered_by(display: &str) -> Vec<Offered> {
    MODES
        .lock()
        .unwrap()
        .iter()
        .find(|(name, _)| name == display)
        .map(|(_, modes)| modes.clone())
        .unwrap_or_default()
}

/// Connector, sound-device and network names, kept alive for as long as the
/// process is.
///
/// A [`Setting`] has to name the display, the device or the network it belongs
/// to and stay `Copy`: every row of the bar carries one by value, and the
/// catalogue holding those rows is cloned, walked and compared all over the
/// shell, so an owned `String` in there would ripple out through the model, the
/// layout and the input path. Every one of these names is fixed for as long as
/// the thing it names exists, and they are already alive for the whole session
/// — so they are interned once each and never freed. A headset plugged in and
/// out all afternoon is one name, not one per plug: the sound server calls it
/// the same thing every time, which is the same property that makes the name
/// worth handing back to it.
///
/// Connectors and sound devices are single digits of each. Wireless networks
/// are not, and that is worth being honest about: this table grows by one short
/// string for every network name that has ever been *drawn* in this session,
/// which in a café is a few dozen and on a walk through a block of flats could
/// be a few hundred. It is bounded by somebody standing at the Networks page
/// watching them arrive, which is a few kilobytes at the outside — and it buys
/// a `Copy` setting, which is what every other row in the tree is.
pub(crate) fn intern(name: &str) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut names = NAMES.lock().unwrap();
    if let Some(known) = names.iter().find(|known| **known == name) {
        return known;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    names.push(leaked);
    leaked
}

/// What the clock on the wall says, and which clock that is.
///
/// The night light is the one setting in this tree that is about a *time*, so
/// it is the one that has to ask. Everything it needs is here and nothing more:
/// the hour the schedule is compared against, and enough about the zone to put
/// on the page, because a user whose light did not come on at nine is owed the
/// answer that their machine thinks it is somewhere else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTime {
    /// 0 to 23.
    pub hour: u8,
    /// 0 to 59.
    pub minute: u8,
    /// Days since the first of January, counted from zero — which is what the
    /// solar equations take.
    pub yday: u16,
    /// The year, for the one thing it decides: whether this one has 366 days
    /// in it.
    pub year: i32,
    /// Seconds east of UTC. Summer time is part of it, because it is part of
    /// what the clock in the room says — and because the sun is worked out
    /// against the clock in the room.
    pub offset: i32,
}

impl LocalTime {
    /// Minutes since midnight, which is what a schedule is compared against.
    pub fn minute_of_day(&self) -> u16 {
        self.hour as u16 * 60 + self.minute as u16
    }
}

/// A minute of the day, as a row says it. The twenty-four hour clock, whatever
/// the machine's own locale would print: it is the one form in which "21:00"
/// cannot be the wrong one of two, and a schedule is exactly where that matters.
fn clock_title(minute: u16) -> String {
    let minute = minute.min(MINUTES_IN_DAY - 1);
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

/// The last reading, and when it was taken.
///
/// The hot path asks what hour it is on every pass of the shell's loop, which
/// is thirty times a second whether or not anything is being drawn, and the
/// answer changes once an hour. So it is read a few times a minute instead:
/// far too often to make the page stale, far too rarely to be worth a thought.
static CLOCK: Mutex<Option<(Instant, LocalTime)>> = Mutex::new(None);

/// How long a reading stands for. Short enough that the minute on the page is
/// never visibly wrong, long enough that the loop is not calling into the C
/// library on every frame.
const CLOCK_TTL: Duration = Duration::from_secs(5);

/// What time it is here, as the machine's own C library reads it.
///
/// Through `localtime_r` rather than any arithmetic of this shell's own,
/// because a time zone is not arithmetic: it is a database of political
/// decisions, kept up to date by the distribution, and the one thing the
/// machine already has a correct answer from. Nothing here parses
/// `/etc/localtime` or reads `TZ` — `tzset` does that, including for a session
/// whose zone was changed underneath it.
///
/// `None` when the library cannot answer, which is a machine with no time zone
/// data at all. The caller treats that as "no schedule" rather than as an
/// unsatisfied one; see [`night_light_now`].
pub fn local_time() -> Option<LocalTime> {
    let mut held = CLOCK.lock().unwrap();
    if let Some((taken, reading)) = held.as_ref() {
        if taken.elapsed() < CLOCK_TTL {
            return Some(reading.clone());
        }
    }
    let reading = read_local_time()?;
    *held = Some((Instant::now(), reading.clone()));
    Some(reading)
}

/// The reading itself, uncached.
fn read_local_time() -> Option<LocalTime> {
    // SAFETY: `time` with a null pointer returns the value rather than storing
    // it, which is what the null is for.
    let now = unsafe { libc::time(std::ptr::null_mut()) };
    if now == -1 {
        return None;
    }
    // glibc's `localtime_r` establishes the zone on its first call and then
    // never looks again, so a session whose machine had its time zone changed
    // underneath it would keep the old one until it was restarted. `tzset` is
    // what re-reads it, and it is declared here because the `libc` crate only
    // binds it on Windows. It is thread-safe and cheap; the library does the
    // caching.
    extern "C" {
        fn tzset();
    }
    // SAFETY: no arguments, no return value, and nothing of ours is borrowed
    // across it.
    unsafe { tzset() };

    let mut broken: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `now` is a valid `time_t` and `broken` is a live, owned `tm`
    // this call is the only writer of.
    let filled = unsafe { libc::localtime_r(&now, &mut broken) };
    if filled.is_null() {
        return None;
    }

    Some(LocalTime {
        // A leap second is `tm_sec == 60` and never an hour of 24, but the
        // clamps cost nothing and this is the one value a schedule is compared
        // against.
        hour: broken.tm_hour.clamp(0, 23) as u8,
        minute: broken.tm_min.clamp(0, 59) as u8,
        yday: broken.tm_yday.clamp(0, 365) as u16,
        // `tm_year` counts from 1900, which is the one thing about `struct tm`
        // everybody knows and everybody forgets.
        year: broken.tm_year + 1900,
        offset: broken.tm_gmtoff.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
    })
}

/// What the sun is doing here today, if anything on this machine says where
/// here is. `None` is a machine with no zone coordinates — see [`crate::sun`].
pub fn sun_today() -> Option<crate::sun::Sun> {
    let now = local_time()?;
    let at = crate::sun::location()?;
    Some(crate::sun::sun(now.yday, now.year, &at, now.offset))
}

/// The rows of the Settings column, in the order they appear under it.
///
/// The picture before the sound, which is the order a console has always put
/// them in and the order the two are noticed in. Appearance stands in front of
/// both because it is the shell describing itself rather than the machine.
///
/// Bluetooth follows Network because the two are one question asked twice — what
/// is this machine talking to — and because the answer to the second is usually
/// something the user is holding. It is second of the pair for the reason
/// Network is fourth: a console can be used without it, and it cannot be used
/// without a picture.
///
/// Input follows the pair for the same reason it follows them on the desk: the
/// two above it are what the machine is talking *to*, and this is what the
/// person in front of it is talking to the machine *with*. It is behind
/// Bluetooth in particular because the control most of these settings are
/// about is the one that arrives over it.
///
/// Games follows Input because it is about neither the machine nor the
/// shell but the programs the console is for — and it is in front of System
/// because a setting about the games is one somebody came looking for.
///
/// Users follows Games because it is the first page here about *people* rather
/// than about the machine or the programs on it — and it is in front of System
/// for the reason Games is: somebody who came to make an account for the person
/// they live with came looking for it, and a page somebody comes looking for
/// should not be behind the one nobody does.
///
/// System comes last because it is the one page about neither: what a display
/// is doing and what the speakers are doing are things the user can point at,
/// and how large the programs on the machine draw themselves is a setting they
/// go looking for.
pub fn column(bar: &[crate::apps::Column]) -> Vec<Entry> {
    vec![
        appearance(),
        display(),
        sounds(),
        network(),
        bluetooth(),
        input(),
        games(),
        users(),
        system(bar),
    ]
}

/// Replace the Settings column in a catalogue with a freshly built one.
///
/// Everything else in the bar comes off the filesystem and is scanned once.
/// This column does not: most of it describes hardware, and a display that
/// turns out not to do HDR — or one plugged in halfway through the session
/// that does — changes which screens are listed under it and what each of them
/// offers. Rebuilding is cheaper and far less error-prone than reaching in to
/// patch a row.
///
/// A cursor standing inside the part that changed shape is left where it is
/// and clamped by the bar, which is the same thing that happens when an
/// application is uninstalled while its row is selected.
pub fn refresh(categories: &mut [Category]) {
    // The columns there are, taken before the one being rebuilt is reached for:
    // one row of this tree is a list of them — see [`startup_category_page`] —
    // and a column cannot be handed the bar it is standing in. Nothing here
    // borrows the catalogue; a [`crate::apps::Column`] is three static names.
    let bar: Vec<crate::apps::Column> = categories.iter().map(Category::named).collect();
    let (id, ..) = crate::apps::SHELL_SETTINGS;
    if let Some(settings) = categories.iter_mut().find(|category| category.id == id) {
        let mut worn = std::mem::take(&mut settings.entries);
        let mut fresh = column(&bar);
        carry_over_listings(&mut worn, &mut fresh);
        settings.entries = fresh;
    }
}

/// Move whatever came off the disk out of the column about to be thrown away.
///
/// One row in this tree is not the tree's to rebuild: the wallpaper picker,
/// whose columns are a `readdir` of wherever the user has walked to rather than
/// something [`column`] can write. Everything else here is rebuilt from the live
/// settings several times a minute — a network appearing, a device pairing, the
/// battery moving — and a rebuild that dropped those rows would empty the column
/// somebody is standing in and throw the cursor back out of it, in the middle of
/// choosing a picture.
///
/// The same lift [`crate::apps::carried_media`] does for the shelves when the
/// catalogue is rescanned, and for the same reason. Matched by title, because
/// that is what a row of this tree is: the shape of the page can change between
/// two rebuilds — a battery row appears, a network goes — and the position of a
/// row cannot be relied on where the position is the thing that moved.
///
/// The *note* is deliberately not carried. It is rebuilt from the setting, which
/// is where the name of the chosen file comes from, and taking the old one would
/// mean a row that went on describing a listing after the choice was made.
fn carry_over_listings(worn: &mut [Entry], fresh: &mut [Entry]) {
    for entry in fresh.iter_mut() {
        let Entry::Folder(folder) = entry else {
            continue;
        };
        let Some(Entry::Folder(same)) = worn
            .iter_mut()
            .find(|worn| worn.title() == folder.title.as_str())
        else {
            continue;
        };
        if folder.place.is_some() {
            folder.entries = std::mem::take(&mut same.entries);
        } else {
            carry_over_listings(&mut same.entries, &mut folder.entries);
        }
    }
}

/// How the shell looks: the colour everything chosen is drawn in, how much
/// material it is drawn with, and — on a machine that has a battery — whether
/// its corner writes the charge out.
///
/// The accent first, because it is the whole shell and what somebody who opens
/// this page came for. The theme second: it is the larger change of the two, and
/// it is also the one nobody goes looking for until something is slow. The
/// battery's figures last, because they are one mark on one screen.
fn appearance() -> Entry {
    let mut rows = vec![accent_colour(), theme_row()];
    // Only on a machine that has one. A desktop offered a switch for battery
    // figures would be offered a setting it can never see the effect of, which
    // is worse than not being offered it: the user would turn it on and go
    // looking for what changed.
    if let Some(charge) = *BATTERY.lock().unwrap() {
        rows.push(battery_percent_switch(charge));
    }
    folder(
        "Appearance",
        "How the shell looks",
        icons::SETTING_APPEARANCE,
        rows,
    )
}

/// The battery's charge in figures, on or off.
///
/// Off and On in that order and marked the way every other switch in this tree
/// is — see [`start_music_switch`], which is the same question asked about the
/// music.
///
/// The row is drawn with the level the machine is actually at, so the list
/// somebody opens is headed by the mark they are deciding about rather than by
/// a picture of a full battery this machine may be nowhere near. It is the one
/// row in this tree whose glyph moves, and it moves for the same reason the
/// accent rows are each painted in the colour they stand for.
fn battery_percent_switch(charge: crate::power::Charge) -> Entry {
    let on = battery_percent();
    folder(
        "Battery percentage",
        "Write the charge out beside the clock",
        crate::ui::battery_glyph(charge),
        vec![
            value("Off", None, !on, Setting::BatteryPercent(false)),
            value("On", None, on, Setting::BatteryPercent(true)),
        ],
    )
}

/// The theme: how much material the shell draws itself with.
///
/// A page rather than a list of values, because there are two questions here and
/// they are not one question. The wallpaper is a long function evaluated for
/// every pixel of every screen on every frame; a mark is a few dozen pixels on a
/// settings row. They cost their own money, and a machine that cannot afford the
/// first can very well afford the second — so a user who came here because
/// something is slow can stand the water down and keep the marks, and a user who
/// came here because they prefer flat marks can have those over the water.
///
/// The wallpaper first: it is the whole screen, and it is the expensive half.
fn theme_row() -> Entry {
    folder(
        "Theme",
        "What the shell is made of",
        icons::SETTING_THEME,
        theme::PARTS.iter().copied().map(material_row).collect(),
    )
}

/// One half of the theme: the wallpaper, or every mark the shell draws.
///
/// Two materials each, and the second exists for one reason above all others — a
/// machine that cannot afford the first. So the comments say what each *costs*
/// as well as what it looks like: a user who is here is here because something
/// is slow, and "the shell's own look" tells them nothing they can act on.
///
/// The wallpaper has a third row, and it is not a material at all: a picture or
/// a film of the user's own, standing where the scene would be. It is last
/// because it is the one answer that is not about this shell — the two above it
/// are what LineXinBar looks like, and this is what somebody's own screen looks
/// like — and because it is the only one that asks a further question. See
/// [`custom_wallpaper_row`].
///
/// The row marked is the applied one rather than the one being previewed, like
/// every other list of values in this tree: what is drawn on screen while the
/// cursor walks is the preview, and what is ticked is the setting.
///
/// The two material rows carry the *same* drawing — the mark at the head of
/// this row, the one that stands for the thing being changed — and are told
/// apart by being drawn in the material each of them applies. That is not a
/// shortage of drawings. The difference between Default and Simple is not a
/// difference of shape: it is the same silhouette read at the same edge, beaded
/// out of its own distance field or laid down flat, so a pair of drawings could
/// only ever have *described* the difference where this one shows it. It is the
/// argument the accent's swatches make, asked of a material — see
/// [`crate::apps::Choice::material`] and [`crate::gpu::Quad::mark`].
///
/// Which also means these four rows are the one place in the shell where a mark
/// is drawn in something other than the theme in force. That is on purpose and
/// it is the same liberty a swatch takes: a column of colours shows five, and
/// only one of them is the accent.
fn material_row(part: theme::Part) -> Entry {
    let in_force = theme::applied_style(part).name();
    let (comment, icon, of_default, of_simple) = match part {
        theme::Part::Wallpaper => (
            "The picture behind everything",
            icons::SETTING_WALLPAPER,
            "A band of water, lit as three sheets",
            "Fine ribbons, for a machine with little to spare",
        ),
        theme::Part::Icons => (
            "Every mark the shell draws",
            icons::SETTING_ICONS,
            "Every mark a bead of water",
            "Flat shapes, for a machine with little to spare",
        ),
    };
    folder(
        part.title(),
        comment,
        icon,
        part.styles()
            .iter()
            .map(|name| match wallpaper::style(name) {
                wallpaper::Style::Custom => custom_wallpaper_row(*name == in_force),
                style => material_value(
                    name,
                    match style {
                        wallpaper::Style::Simple => of_simple,
                        _ => of_default,
                    },
                    icon,
                    style,
                    *name == in_force,
                    Setting::Style(part, name),
                ),
            })
            .collect(),
    )
}

/// The third answer under Wallpaper: one of the user's own pictures or films,
/// drawn instead of the shell's scene.
///
/// A subcategory that is also the answer — the second row in the shell built
/// that way, after the wireless network a radio is on, and for the same reason
/// [`crate::apps::Folder::chosen`] gives. It is an answer to the question its
/// column asks, and it is also the only honest place to ask *which* picture:
/// the value is a file, there is no list of files to put in the column
/// beforehand, and a row that opened a picker without being markable would leave
/// the column with nothing ticked while a photograph was on the screen.
///
/// What hangs under it is the disk itself — the same explorer as Files, walked
/// the same way, listing only what could stand behind a screen. See
/// [`crate::files::Shows`].
///
/// The note is the file that is being shown, where there is one: somebody
/// coming back to this row a month later wants to know which picture they
/// chose, and the row above it in the column already says what the alternative
/// is.
fn custom_wallpaper_row(chosen: bool) -> Entry {
    let comment = custom_wallpaper()
        .and_then(|file| {
            file.file_name()
                .and_then(std::ffi::OsStr::to_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| "A picture or a film of your own".to_string());
    let Entry::Folder(mut inner) = folder(
        wallpaper::CUSTOM,
        &comment,
        // The mark of a picture, which is the mark every photograph on this bar
        // already wears — a wallpaper of somebody's own is one of those rather
        // than a sixth kind of setting. Nothing new is drawn for it, which is
        // also what keeps one object looking like itself across the shell.
        icons::CATEGORY_IMAGES,
        Vec::new(),
    ) else {
        unreachable!("folder builds a folder");
    };
    inner.chosen = chosen;
    // Read on the press that opens it rather than now: what is on somebody's
    // disk is a question about the moment they ask it, and building this
    // column with the rest of the settings tree would walk their home directory
    // every time any setting anywhere changed.
    inner.place = Some(crate::files::Place::Volumes(crate::files::Shows::Scenery));
    Entry::Folder(inner)
}

/// The accent: the colour of the selection glow, the lit rim of a chosen pane,
/// and the bloom under the icon the cursor is on.
///
/// The list is [`theme::ACCENTS`] itself rather than a second copy of it
/// written out here, and each row is drawn in the colour it stands for, taken
/// from that same palette — so a retheme cannot leave this list describing a
/// shell that no longer exists. The row marked is the one in force at the
/// moment the column is built, which is why the setting is read from disk
/// before the catalogue is assembled.
fn accent_colour() -> Entry {
    let in_force = theme::accent().name;
    folder(
        "Accent color",
        "The colour of being chosen",
        icons::SETTING_ACCENT,
        theme::ACCENTS
            .iter()
            .map(|accent| {
                swatch(
                    accent.name,
                    accent.theme.accent,
                    accent.name == in_force,
                    Setting::Accent(accent.name),
                )
            })
            .collect(),
    )
}

/// What leaves the machine and reaches the screen, as opposed to what the
/// shell draws. Everything under here is carried out by the compositor.
///
/// The mode comes first, in its two halves, because it is the plainest thing
/// about a display and the one a user is most likely to have come here for;
/// the orientation is the other thing about the picture's shape, and stands
/// with them. The order comes after those three because it is the one page
/// here that is not about a single screen's picture at all — it is about where
/// the screens stand relative to one another — and a user with one display
/// never needs it. The last two describe the picture those carry, and in that
/// order: the night light is the one every display can do and the one somebody
/// comes looking for at ten in the evening, HDR is the one only some hardware
/// has.
fn display() -> Entry {
    folder(
        "Display",
        "How the picture reaches the screen",
        icons::SETTING_DISPLAY,
        vec![
            resolution(),
            refresh_rate(),
            orientation(),
            display_order(),
            night_light(),
            high_dynamic_range(),
            oled_protection(),
        ],
    )
}

/// OLED protection: rest a screen nobody is watching while another one is
/// being used.
///
/// Last of the Display pages, and it earns that place the way HDR earns
/// second-to-last: the ones above it are about the picture every display has,
/// and this is about the *panel* — a question only some hardware makes anybody
/// ask, and one nobody comes to this column looking for until they own the
/// screen that needs it.
///
/// The same three shapes as [`night_light`] and [`high_dynamic_range`], and for
/// the same reasons. What differs is what is left out, which is nothing: a
/// black sheet is the compositor's own drawing over a display it is already
/// driving, so every screen it reports can be rested. A whole session drops off
/// this page instead — one whose compositor has never heard of the request —
/// and there the row says so rather than opening onto a switch that would do
/// nothing.
fn oled_protection() -> Entry {
    let screens = support();
    if !screen_rest_available() || screens.is_empty() {
        return folder(
            "OLED protection",
            "Rest a screen nobody is watching",
            icons::SETTING_SCREEN_REST,
            vec![nothing_can_be_rested()],
        );
    }
    match screens.as_slice() {
        // One screen, so there is no screen to choose — and the page above it
        // says which screen it is and what the switch is set to.
        [(name, _)] => folder(
            "OLED protection",
            &format!("{name} — {}", resting_of(name)),
            icons::SETTING_SCREEN_REST,
            oled_protection_controls(name),
        ),
        _ => folder(
            "OLED protection",
            "Rest a screen nobody is watching",
            icons::SETTING_SCREEN_REST,
            screens
                .iter()
                .map(|(name, _)| {
                    folder(
                        name,
                        &resting_of(name),
                        icons::SETTING_DISPLAY,
                        oled_protection_controls(name),
                    )
                })
                .collect(),
        ),
    }
}

/// The controls, for one screen — shared by the screen list and by the session
/// that has only one screen, the way [`night_light_controls`] is.
///
/// One row, and it is the switch. Nothing else about this is the user's to set:
/// how long a screen is left alone first, how long the fade takes, and what
/// counts as leaving it alone are answers this shell has, and a page offering
/// four of them would be asking the user to design the feature.
fn oled_protection_controls(name: &str) -> Vec<Entry> {
    vec![oled_protection_switch(
        intern(name),
        oled_protection_for(name),
    )]
}

/// What one screen's switch is set to, in the few words a row's comment has.
fn resting_of(display: &str) -> String {
    match oled_protection_for(display) {
        true => "On".to_string(),
        false => "Off".to_string(),
    }
}

/// The switch itself, on or off.
fn oled_protection_switch(display: &'static str, on: bool) -> Entry {
    folder(
        "OLED protection",
        "Fade this screen to black while another screen is being used",
        icons::SETTING_SCREEN_REST,
        vec![
            value(
                "Off",
                None,
                !on,
                setting(display, DisplayValue::OledProtection(false)),
            ),
            value(
                "On",
                None,
                on,
                setting(display, DisplayValue::OledProtection(true)),
            ),
        ],
    )
}

/// The row that stands in for the screen list when no screen can be rested.
fn nothing_can_be_rested() -> Entry {
    reading(
        "No display can be rested",
        "The black is drawn over the whole of a display, cursor and all, so it \
         is the compositor's to draw: this session's has never heard of it",
    )
}

/// Resolution: one subcategory per screen the compositor reports modes for —
/// unless there is only one such screen, in which case its sizes stand here
/// directly.
///
/// The same three shapes as [`high_dynamic_range`], and for the same reasons.
/// What differs is what is left out, which is nothing: every size the connector
/// lists is a row, on every screen that lists any. A screen drops off this page
/// only when there is no mode list behind it at all — a nested session, where
/// the size of the window belongs to the compositor LineXinBar is running inside.
///
/// Sizes rather than modes, and the rate on its own page, because they are two
/// questions: a list that spelled out every combination would be twenty rows
/// on an ordinary television, most of them the same six sizes over again.
/// Splitting them puts the size first, which is the order they belong in — the
/// rate page is a page about whatever size this one has settled on.
fn resolution() -> Entry {
    let listed: Vec<(String, Vec<Offered>)> = modes()
        .into_iter()
        .filter(|(_, modes)| !modes.is_empty())
        .collect();

    match listed.as_slice() {
        // Never empty, as the HDR page is never empty: a subcategory the bar
        // refuses to step into is a row that does nothing when pressed.
        [] => folder(
            "Resolution",
            "How many pixels the picture is",
            icons::SETTING_RESOLUTION,
            vec![nothing_reports_modes()],
        ),
        // One screen, so there is no screen to choose: the sizes take the place
        // of the list, and the row above them says which screen they belong to
        // and what it is showing.
        [(name, modes)] => folder(
            "Resolution",
            &format!("{name} — {}", running_size(modes)),
            icons::SETTING_RESOLUTION,
            size_values(name, modes),
        ),
        _ => folder(
            "Resolution",
            "How many pixels the picture is",
            icons::SETTING_RESOLUTION,
            listed
                .iter()
                .map(|(name, modes)| {
                    folder(
                        name,
                        &running_size(modes),
                        icons::SETTING_DISPLAY,
                        size_values(name, modes),
                    )
                })
                .collect(),
        ),
    }
}

/// Refresh rate: the same page again, for the other half of a mode.
///
/// The rates of the size that screen is set to, and of no other size. A refresh
/// rate is not a thing a display has on its own — it is a thing a *mode* has,
/// and 240 Hz at 1440p is not 240 Hz at 1080p. Listing every rate the connector
/// mentions would put rates on the page that the size in force cannot be given,
/// and choosing one would have to move the size out from under the Resolution
/// page to keep its word. So the rate is the size's child: pick the size first,
/// and this page offers what that size can be refreshed at.
///
/// Which means a rate never moves the size. Only the Resolution page does that,
/// and it carries the rate along where the new size has it.
fn refresh_rate() -> Entry {
    let listed: Vec<(String, Vec<Offered>)> = modes()
        .into_iter()
        .filter(|(name, modes)| !offered_rates(name, modes).is_empty())
        .collect();

    match listed.as_slice() {
        [] => folder(
            "Refresh rate",
            "How often the picture is redrawn",
            icons::SETTING_REFRESH,
            vec![nothing_reports_modes()],
        ),
        [(name, modes)] => folder(
            "Refresh rate",
            &format!("{name} — {}", running_rate(name, modes)),
            icons::SETTING_REFRESH,
            rate_values(name, modes),
        ),
        _ => folder(
            "Refresh rate",
            "How often the picture is redrawn",
            icons::SETTING_REFRESH,
            listed
                .iter()
                .map(|(name, modes)| {
                    folder(
                        name,
                        &running_rate(name, modes),
                        icons::SETTING_DISPLAY,
                        rate_values(name, modes),
                    )
                })
                .collect(),
        ),
    }
}

/// Orientation: which way up each screen's picture is drawn.
///
/// The same three shapes as the two pages above it, and for the same reasons —
/// one screen and there is no screen to choose, several and they are named
/// first, none and the row says why rather than opening onto nothing.
///
/// The screens are the ones the compositor says it turns itself. That is not
/// the same list as the one on the Resolution page and must not be derived
/// from it: a turn is the compositor's own drawing rather than anything the
/// connector has to support, so a display with no mode list can still be
/// turned, and one the compositor is not the last word on cannot be — which is
/// exactly what it reports.
///
/// Every turn is offered on every screen, unlike a mode: there is no list of
/// orientations a display has, because none of it reaches the hardware. What a
/// screen is at is what the compositor says it is drawing.
fn orientation() -> Entry {
    let listed = turned();

    match listed.as_slice() {
        [] => folder(
            "Orientation",
            "Which way up the picture is",
            icons::SETTING_ORIENTATION,
            vec![nothing_can_be_turned()],
        ),
        [(name, turn)] => folder(
            "Orientation",
            &format!("{name} — {}", turn.title()),
            icons::SETTING_ORIENTATION,
            turn_values(name, *turn),
        ),
        _ => folder(
            "Orientation",
            "Which way up the picture is",
            icons::SETTING_ORIENTATION,
            listed
                .iter()
                .map(|(name, turn)| {
                    folder(
                        name,
                        turn.title(),
                        icons::SETTING_DISPLAY,
                        turn_values(name, *turn),
                    )
                })
                .collect(),
        ),
    }
}

/// The four turns, for one screen.
///
/// The mark is on what the compositor says it is drawing, not on what was last
/// asked for — the rule the Resolution page follows, and here the answer comes
/// back in the same breath as well. A screen the config has put into one of
/// the mirrored orientations therefore has no row marked, which is the truth:
/// it is not in any of these, and the row above the list says which one it is
/// in.
fn turn_values(name: &str, turned: Orientation) -> Vec<Entry> {
    let display = intern(name);
    ROTATIONS
        .iter()
        .map(|turn| {
            drawn_value(
                turn.title(),
                Some(turn.note()),
                // Never the bead: see [`icons::SETTING_ROTATION_0`]. A turn is
                // the one setting in this tree whose value has a shape, and
                // four identical beads would throw that away.
                turn.icon().unwrap_or(icons::SWATCH),
                *turn == turned,
                setting(display, DisplayValue::Orientation(*turn)),
            )
        })
        .collect()
}

/// The row that stands in for the screen list when nothing can be turned.
fn nothing_can_be_turned() -> Entry {
    reading(
        "No display can be turned",
        "Nothing here owns its own picture: the session is running inside \
         another compositor, which owns which way up its window is",
    )
}

/// Display order: which screen the compositor puts first, which second, and so
/// on down the row it lays them out in.
///
/// The one page under Display that is about the screens rather than about a
/// screen. Everything else here answers "what is this display doing"; this
/// answers "which of these is the first one", which is what decides where a
/// pointer leaving one screen's edge comes out and which way along the desk
/// the windows go.
///
/// Two of the three shapes the pages above it have, and deliberately not the
/// third: a single screen does not collapse to a list of places, because there
/// is no list. One display is the whole arrangement, and the row says so
/// rather than offering the user the one thing they already have.
///
/// The screens are the ones the compositor says it arranges, which is neither
/// the Resolution page's list nor the Orientation page's: a display pinned to a
/// position by the compositor's own config takes no part in the arrangement,
/// and on a session mirroring every screen onto one region there is no
/// arrangement to take part in.
///
/// They are listed in the order they were announced rather than in the order
/// they stand, so that the rows hold still while the arrangement changes under
/// them — see [`PLACED`], which is the whole of that argument.
fn display_order() -> Entry {
    let listed = placed();
    let order = arrangement();

    match listed.as_slice() {
        [] => folder(
            "Display order",
            "Which screen comes first",
            icons::SETTING_ORDER,
            vec![nothing_can_be_arranged()],
        ),
        [(only, _)] => folder(
            "Display order",
            &format!("{only} — the only screen"),
            icons::SETTING_ORDER,
            vec![one_screen_is_the_whole_arrangement()],
        ),
        _ => folder(
            "Display order",
            "Which screen comes first",
            icons::SETTING_ORDER,
            listed
                .iter()
                .map(|(name, standing)| {
                    folder(
                        name,
                        &place_title(*standing as usize),
                        icons::SETTING_DISPLAY,
                        place_values(name, *standing, &order),
                    )
                })
                .collect(),
        ),
    }
}

/// The places on offer, for one screen: as many as there are screens.
///
/// The mark is on where the compositor says this screen *is*, not on where it
/// was last asked to be — the rule the Resolution and Orientation pages follow,
/// and here as there the answer comes back in the same breath as the change.
///
/// Each row that is not the marked one says which screen is standing there,
/// because that is what pressing it does: the two trade. A list of screens has
/// no empty places to move into, so an order can only ever be changed by
/// exchanging two of them, and a page that said "Display 1" without saying who
/// was leaving it would be hiding half of what the press does.
fn place_values(name: &str, standing: u32, order: &[String]) -> Vec<Entry> {
    let display = intern(name);
    order
        .iter()
        .enumerate()
        .map(|(place, holder)| {
            let here = place as u32 == standing;
            let note = match here {
                true => "Where this screen is now".to_string(),
                false => format!("Trades places with {holder}"),
            };
            value(
                &place_title(place),
                Some(&note),
                here,
                setting(display, DisplayValue::Place(place as u32)),
            )
        })
        .collect()
}

/// What one place is called: the user's own name for it.
///
/// Counted from one, unlike everything under the page — the protocol, the
/// setting and the file all count places from zero, because they are indices
/// into a list. Nobody calls their leftmost monitor the zeroth one.
fn place_title(place: usize) -> String {
    format!("Display {}", place + 1)
}

/// The row that stands in for the screen list when nothing has a place.
fn nothing_can_be_arranged() -> Entry {
    reading(
        "No display can be moved",
        "Nothing here has a place to change: the session is running inside \
         another compositor, or every screen is set to show the same region, \
         and neither has a first screen to be",
    )
}

/// And the row for the ordinary machine: one screen, which is already the
/// whole of the order it is in.
fn one_screen_is_the_whole_arrangement() -> Entry {
    reading(
        "Only one display",
        "An order is something two screens have. This one is the whole \
         arrangement, and there is nowhere else in it to stand",
    )
}

/// The sizes of one screen, largest first.
///
/// Sorted rather than left in the connector's order, which is the driver's and
/// is not promised to be anything: a list of resolutions that does not descend
/// is a list nobody can scan.
///
/// The mark is on the size the display is *actually* running, not on the one
/// last asked for. A mode the hardware refused must not read as chosen, and
/// unlike a colour there is no waiting for it: the answer comes back in the
/// same breath as the change.
fn size_values(name: &str, modes: &[Offered]) -> Vec<Entry> {
    let display = intern(name);
    let running = running(modes).map(|mode| mode.resolution);
    sizes(modes)
        .into_iter()
        .map(|resolution| {
            value(
                &pixels(resolution),
                Some(&fastest_at(modes, resolution)),
                running == Some(resolution),
                setting(display, DisplayValue::Resolution(resolution)),
            )
        })
        .collect()
}

/// The rates one screen's size in force carries, fastest first.
///
/// The mark goes on the whole mode rather than on the rate alone: these rates
/// belong to one size, so a display running some other size is not running any
/// of them, however well the number matches.
fn rate_values(name: &str, modes: &[Offered]) -> Vec<Entry> {
    let display = intern(name);
    let Some(showing) = size_shown(name, modes) else {
        return Vec::new();
    };
    let running = running(modes);
    let rates = rates_at(modes, showing);
    let titles = rate_titles(&rates);
    rates
        .into_iter()
        .zip(titles)
        .map(|((refresh, preferred), title)| {
            value(
                &title,
                // Only the display's own rate has anything to add. The rest are
                // a number of hertz, which the title already is, and a comment
                // restating it is noise on every row.
                preferred.then_some("What this display asks for"),
                running
                    == Some(Mode {
                        resolution: showing,
                        refresh,
                    }),
                setting(display, DisplayValue::RefreshRate(refresh)),
            )
        })
        .collect()
}

/// What to call each rate in a list of them, kept distinct.
///
/// Two modes a display really does offer can round to the same friendly rate —
/// 119.998 and 120.000 are both "120 Hz", and a 1440p240 monitor lists both.
/// Neither may be dropped, and two rows a page cannot tell apart are worse than
/// a number with three decimals in it, so where that happens every row in the
/// clash is printed at the precision the value is held in: thousandths of a
/// hertz, which is what the mode list itself is counted in. Every other row
/// keeps the short form.
fn rate_titles(rates: &[(u32, bool)]) -> Vec<String> {
    let short: Vec<String> = rates
        .iter()
        .map(|(refresh, _)| hertz(*refresh).unwrap_or_else(|| "Unreported".to_string()))
        .collect();
    short
        .iter()
        .enumerate()
        .map(|(index, title)| {
            let clashes = short
                .iter()
                .enumerate()
                .any(|(other, seen)| other != index && seen == title);
            match clashes {
                true => format!("{}.{:03} Hz", rates[index].0 / 1000, rates[index].0 % 1000),
                false => title.clone(),
            }
        })
        .collect()
}

/// The mode a display is being driven at, if the compositor has said yet.
fn running(modes: &[Offered]) -> Option<Mode> {
    modes
        .iter()
        .find(|offered| offered.current)
        .map(|offered| offered.mode)
}

/// The distinct sizes a screen offers, largest first.
fn sizes(modes: &[Offered]) -> Vec<Resolution> {
    let mut sizes: Vec<Resolution> = Vec::new();
    for offered in modes {
        if !sizes.contains(&offered.mode.resolution) {
            sizes.push(offered.mode.resolution);
        }
    }
    sizes.sort_by_key(|size| {
        std::cmp::Reverse((u64::from(size.width) * u64::from(size.height), size.width))
    });
    sizes
}

/// The distinct rates one size carries, fastest first, each with whether the
/// display's own mode is that size at that rate.
///
/// A mode reporting no rate at all — which a virtual display may — is not a
/// rate, and is left out rather than listed as nothing. It is still a size, and
/// the Resolution page still has it.
fn rates_at(modes: &[Offered], resolution: Resolution) -> Vec<(u32, bool)> {
    let mut rates: Vec<(u32, bool)> = Vec::new();
    let carried = modes
        .iter()
        .filter(|offered| offered.mode.resolution == resolution && offered.mode.refresh != 0);
    for offered in carried {
        match rates
            .iter_mut()
            .find(|(refresh, _)| *refresh == offered.mode.refresh)
        {
            Some((_, preferred)) => *preferred |= offered.preferred,
            None => rates.push((offered.mode.refresh, offered.preferred)),
        }
    }
    rates.sort_by_key(|(refresh, _)| std::cmp::Reverse(*refresh));
    rates
}

/// The mode a screen is set to: what it was last asked for, and failing that
/// what it is running.
///
/// The request comes first because it is the half a row has to fill in, and a
/// user who has just chosen 1440p is owed 1440p's rates on the next page rather
/// than the ones belonging to whatever was there before. The display's own mode
/// stands in until something has been chosen, which for most sessions is
/// always.
fn wanted(display: &str, modes: &[Offered]) -> Option<Mode> {
    mode_for(display).or_else(|| running(modes))
}

/// The size a screen's rates are listed for.
///
/// The largest it has where nothing is known at all, so that a screen reporting
/// modes never falls off the Refresh rate page: a display the compositor has
/// not said anything about yet still has rates worth showing, and an empty page
/// would read as a display that cannot be refreshed.
fn size_shown(display: &str, modes: &[Offered]) -> Option<Resolution> {
    wanted(display, modes)
        .map(|mode| mode.resolution)
        .or_else(|| sizes(modes).first().copied())
}

/// The rates that end up under one screen on the Refresh rate page.
fn offered_rates(display: &str, modes: &[Offered]) -> Vec<(u32, bool)> {
    size_shown(display, modes)
        .map(|size| rates_at(modes, size))
        .unwrap_or_default()
}

/// What size a screen is showing, in the few words a row's comment has.
fn running_size(modes: &[Offered]) -> String {
    match running(modes) {
        Some(mode) => pixels(mode.resolution),
        None => format!("{} sizes to choose from", sizes(modes).len()),
    }
}

/// What rate it is showing it at, and at what size.
///
/// The size is on this row because the rates underneath are that size's alone:
/// a page of rates with no size named would be a page whose contents change for
/// a reason it never gives.
fn running_rate(display: &str, modes: &[Offered]) -> String {
    let Some(size) = size_shown(display, modes) else {
        return "No refresh rate reported".to_string();
    };
    // The rate has to be the display's *at that size*: a screen that has been
    // asked for a size it is not on yet is not running any of the rates this
    // page is about, and naming one of them would be naming another size's.
    let running = running(modes)
        .filter(|mode| mode.resolution == size)
        .and_then(|mode| hertz(mode.refresh));
    match running {
        Some(rate) => format!("{rate} at {}", pixels(size)),
        None => format!("{} rates at {}", rates_at(modes, size).len(), pixels(size)),
    }
}

/// The row that stands in for a screen list with no screen to list.
fn nothing_reports_modes() -> Entry {
    reading(
        "No display reports its modes",
        "Nothing here owns a connector: the session is running inside another \
         compositor, which owns the size of its window",
    )
}

/// A size, as a row is titled with it.
fn pixels(resolution: Resolution) -> String {
    format!("{} × {}", resolution.width, resolution.height)
}

/// The line under a size: the best it can be refreshed at, and whether it is
/// the display's own.
///
/// The fastest rather than all of them, because the rates are the next row's
/// question and a comment listing five of them would answer it here badly. It
/// is still what tells 1080p at 144 from 1440p at 60, which is exactly the
/// choice this page is for.
fn fastest_at(modes: &[Offered], resolution: Resolution) -> String {
    let fastest = rates_at(modes, resolution)
        .into_iter()
        .map(|(refresh, _)| refresh)
        .max();
    let native = modes
        .iter()
        .any(|offered| offered.preferred && offered.mode.resolution == resolution);
    match (fastest.and_then(hertz), native) {
        (Some(rate), true) => format!("Up to {rate}, and what this display asks for"),
        (Some(rate), false) => format!("Up to {rate}"),
        (None, true) => "What this display asks for".to_string(),
        (None, false) => "No refresh rate reported".to_string(),
    }
}

/// A refresh rate in mHz as a rate somebody would say out loud.
///
/// `None` for a mode with no rate at all, which a virtual display may report
/// and which is not the same as zero hertz.
fn hertz(refresh: u32) -> Option<String> {
    if refresh == 0 {
        return None;
    }
    if refresh % 1000 == 0 {
        return Some(format!("{} Hz", refresh / 1000));
    }
    // Two decimals is what tells 59.94 from 60; the trailing zero of a rate
    // like 74.9 is noise.
    let rate = format!("{:.2}", refresh as f64 / 1000.0);
    let rate = rate.trim_end_matches('0').trim_end_matches('.');
    Some(format!("{rate} Hz"))
}

/// Night light: one subcategory per screen whose picture can be warmed —
/// unless there is only one, in which case its settings stand here directly.
///
/// The same three shapes as [`high_dynamic_range`] and for the same reasons,
/// but a much longer list of screens: warming a picture is a scaled gamma ramp
/// and nothing else, so every display driving a real connector can do it. What
/// drops off is a session running nested inside another compositor, which owns
/// no ramp — and there the row says so rather than opening onto a page whose
/// every control would be inert.
///
/// Per screen, like everything else under Display, and the schedule with it. A
/// television across a lit room and a laptop panel a foot from the eye are not
/// the same question, and this page holds to the rule the rest of the tree does
/// rather than making one exception for the one setting that mentions a clock.
fn night_light() -> Entry {
    let capable: Vec<(String, Support)> = support()
        .into_iter()
        .filter(|(_, support)| support.night_light)
        .collect();

    match capable.as_slice() {
        [] => folder(
            "Night light",
            "Blue light filter",
            icons::SETTING_NIGHT_LIGHT,
            vec![nothing_can_be_warmed()],
        ),
        [(name, support)] => folder(
            "Night light",
            &format!("{name} — {}", warmth_of(name, *support)),
            icons::SETTING_NIGHT_LIGHT,
            night_light_controls(name),
        ),
        _ => folder(
            "Night light",
            "Blue light filter",
            icons::SETTING_NIGHT_LIGHT,
            capable
                .iter()
                .map(|(name, support)| {
                    folder(
                        name,
                        &warmth_of(name, *support),
                        icons::SETTING_DISPLAY,
                        night_light_controls(name),
                    )
                })
                .collect(),
        ),
    }
}

/// The controls, for one screen.
///
/// Shared by the screen list and by the session that has only one screen, the
/// way [`controls`] is: the page is the same page either way, and only the
/// level above it differs.
///
/// The switch, then how warm, then when — which is the order the questions are
/// asked in and the order they stop mattering in.
///
/// Three rows or five. The two hours are not the schedule's detail so much as
/// *one* schedule's, and on either of the other two they are worse than
/// useless: a page that follows the sun and still shows "From 22:00" is a page
/// making a claim about tonight that is not true, and no wording in the row
/// undoes the two hours sitting there in plain sight. So they are not there —
/// which is also why the column has to be rebuilt when this row is answered.
fn night_light_controls(name: &str) -> Vec<Entry> {
    let display = intern(name);
    let night = night_light_for(display);
    let mut rows = vec![
        night_light_switch(display, night),
        night_light_temperature(display, night),
        night_light_schedule(display, night),
    ];
    if night.schedule == Schedule::Hours {
        rows.push(night_light_from(display, night));
        rows.push(night_light_until(display, night));
    }
    rows
}

/// What a screen's night light is doing, in the few words a row's comment has.
///
/// It has to say two things at once — whether the picture is warm *now* and
/// what will change that — because "on" is not the answer for a light that is
/// switched on and waiting for the evening. So the moment it next turns on or
/// off is what stands beside the state, which is exactly what a user standing
/// at this row at half past eight wants to know.
fn warmth_of(display: &str, support: Support) -> String {
    let night = night_light_for(display);
    if !night.enabled {
        return "Off".to_string();
    }
    let kelvin = format!("{} K", night.temperature);
    let Some(now) = local_time() else {
        // No clock is no schedule being kept — see `night_light_now` — so
        // saying when it turns on would be saying something untrue.
        return format!("On, {kelvin}");
    };
    let sun = sun_today();
    let burning = night.burning_at(now.minute_of_day(), sun);
    // The compositor's word for whether the picture is actually warm. Its hour
    // having come and the picture not being warm is a request that has not
    // landed yet, or could not, and must not read as success.
    if burning && !support.warming {
        return format!("{}, {kelvin}", schedule_of(night, sun));
    }
    match (burning, night.next_edge(now.minute_of_day(), sun)) {
        (true, Some(off)) => format!("On until {}, {kelvin}", clock_title(off)),
        (true, None) => format!("On, {kelvin}"),
        (false, Some(on)) => format!("On at {}, {kelvin}", clock_title(on)),
        // Switched on, not burning, and nothing will change that today: the
        // midnight sun, which is the one case with an answer of its own.
        (false, None) => format!("Off while the sun is up, {kelvin}"),
    }
}

/// The hours a schedule keeps, said without any claim about now.
fn schedule_of(night: NightLight, sun: Option<crate::sun::Sun>) -> String {
    match night.schedule {
        Schedule::AllDay => "On".to_string(),
        Schedule::Hours => format!("{} to {}", hour_title(night.from), hour_title(night.until)),
        Schedule::SunsetToSunrise => match sun {
            Some(crate::sun::Sun::Daily { sunrise, sunset }) => {
                format!("{} to {}", clock_title(sunset), clock_title(sunrise))
            }
            Some(crate::sun::Sun::NeverRises) => "The sun does not rise today".to_string(),
            Some(crate::sun::Sun::NeverSets) => "The sun does not set today".to_string(),
            None => "No location to work the sun out from".to_string(),
        },
    }
}

/// The row that stands in for the screen list when no picture can be warmed.
fn nothing_can_be_warmed() -> Entry {
    reading(
        "No display can be warmed",
        "Nothing here owns a colour ramp: the session is running inside \
         another compositor, which owns what its window is tinted with",
    )
}

/// The switch itself, on or off — and off is the whole of off: a display whose
/// switch is here is never warmed, whatever the schedule below says.
fn night_light_switch(display: &'static str, night: NightLight) -> Entry {
    let on = night.enabled;
    folder(
        "Night light",
        "Take the blue out of the picture",
        icons::SETTING_NIGHT_LIGHT,
        vec![
            value(
                "Off",
                None,
                !on,
                setting(display, DisplayValue::NightLight(false)),
            ),
            value(
                "On",
                None,
                on,
                setting(display, DisplayValue::NightLight(true)),
            ),
        ],
    )
}

/// How far the picture is warmed — a bar rather than a list, because it is the
/// one setting in this tree whose answers are a *scale*.
///
/// Every hundred kelvin between candlelight and daylight is a sensible answer.
/// As rows that is forty-five of them, which is a column nobody can scan and a
/// list standing for a quantity that has no steps in it to begin with; the
/// short list it replaces was eight arbitrary points, and a user who wanted the
/// one between two of them could not have it. On a bar the whole range is under
/// the cursor at once, Up and Down mean what they mean in every other column,
/// and Left still leaves — so nothing new has to be learnt to use it or to get
/// back out of it.
///
/// The filled part is drawn in the colour of the light it stands for, which is
/// the one thing neither the number nor the words can be: a picture of what the
/// screen is about to look like. Higher up the track is more kelvin, which is
/// cooler and less filter, so a full white bar reads as what it is — no warming
/// at all — and a short orange one as candlelight.
fn night_light_temperature(display: &'static str, night: NightLight) -> Entry {
    let kelvin = night.temperature.clamp(WARMEST_ON_THE_BAR, NEUTRAL_KELVIN);
    let step = |to: u16| {
        (WARMEST_ON_THE_BAR..=NEUTRAL_KELVIN)
            .contains(&to)
            .then(|| setting(display, DisplayValue::NightLightTemperature(to)))
    };
    let span = (NEUTRAL_KELVIN - WARMEST_ON_THE_BAR) as f32;
    folder(
        "Color temperature",
        &format!("{kelvin} K — {}", warmth_note(kelvin).to_lowercase()),
        icons::SETTING_APPEARANCE,
        vec![Entry::Bar(crate::apps::Bar {
            title: format!("{kelvin} K"),
            comment: Some(warmth_note(kelvin).to_string()),
            fill: (kelvin - WARMEST_ON_THE_BAR) as f32 / span,
            swatch: Some(tint_of(kelvin)),
            up: step(kelvin.saturating_add(TEMPERATURE_STEP)),
            down: step(kelvin.saturating_sub(TEMPERATURE_STEP)),
            // The same steps a direction walks, all of them, so a click along
            // the groove reaches the one it landed on directly. Every hundred
            // kelvin of the range in order, which is exactly what `fill`
            // measures the handle's place against.
            steps: (WARMEST_ON_THE_BAR..=NEUTRAL_KELVIN)
                .step_by(TEMPERATURE_STEP as usize)
                .map(|kelvin| setting(display, DisplayValue::NightLightTemperature(kelvin)))
                .collect(),
        })],
    )
}

/// How far one press moves the bar.
///
/// A hundred kelvin is about the smallest step that is visible on a screen at
/// all, so it is the finest one worth being able to ask for — and it puts the
/// whole range twenty-two presses from end to end, which a held direction
/// crosses in a moment.
const TEMPERATURE_STEP: u16 = 100;

/// The warm end of the bar.
///
/// Not [`WARMEST_KELVIN`], which is as far as the compositor will encode.
/// Below roughly 1900 K a black body has no blue in it at all, so the ramp
/// takes that channel to zero — and a screen with no blue channel does not
/// show a blue-on-white page as warm, it shows it as blank. What the bar
/// offers is every temperature that is still a picture.
pub const WARMEST_ON_THE_BAR: u16 = 2000;

/// What a temperature means, in the words a number cannot carry.
///
/// Bands rather than a word per step, because a hundred kelvin is not a
/// difference anybody has a separate name for — and warmer strictly down the
/// list, so a bar walked in one direction never reads as turning back.
///
/// The top band is the head of the track alone. 6500 K is the only temperature
/// at which this filter does nothing whatever, and one step below it is a
/// picture that has been changed, however slightly: a row that said "no warming
/// at all" there would be saying the setting had not taken.
fn warmth_note(kelvin: u16) -> &'static str {
    match kelvin {
        0..=2200 => "Candlelight, and as far as this goes",
        2201..=2900 => "A filament bulb",
        2901..=3600 => "Distinctly warm, like a lamp",
        3601..=4400 => "An ordinary evening",
        4401..=5200 => "Warm, and still easy to read by",
        5201..=6000 => "A little off daylight",
        6001..=6499 => "Barely warm: the gentlest this goes",
        _ => "Daylight: no warming at all",
    }
}

/// The colour a screen shows at `kelvin`, for the filled part of the bar.
///
/// The same closed-form fit to the Planckian locus the compositor builds its
/// ramp from, and deliberately so: this is a *picture* of what that ramp is
/// about to do, and a picture drawn from different arithmetic would be a
/// preview of something else. The compositor remains the authority — it is
/// what clamps, normalises and commits — and nothing here reaches the screen.
fn tint_of(kelvin: u16) -> Color {
    let temperature = kelvin.clamp(1000, 40_000) as f32 / 100.0;
    let red = match temperature <= 66.0 {
        true => 255.0,
        false => 329.698_73 * (temperature - 60.0).powf(-0.133_204_76),
    };
    let green = match temperature <= 66.0 {
        true => 99.470_8 * temperature.ln() - 161.119_57,
        false => 288.122_16 * (temperature - 60.0).powf(-0.075_514_85),
    };
    let blue = if temperature >= 66.0 {
        255.0
    } else if temperature <= 19.0 {
        0.0
    } else {
        138.517_73 * (temperature - 10.0).ln() - 305.044_8
    };
    let byte = |value: f32| value.clamp(0.0, 255.0).round() as u32;
    Color(byte(red) << 16 | byte(green) << 8 | byte(blue))
}

/// When the light burns: never on a schedule, on the sun's, or between two
/// hours the user chose.
///
/// One row with three answers rather than a switch and a pair of hours,
/// because they are three different things to want and a user has to be able
/// to say which without first setting up the other two.
///
/// The sun is only offered where there is a sun to follow. It needs a latitude
/// and a longitude — see [`crate::sun`] — and on a machine that publishes
/// neither the row is replaced by the reason there is no row, which is what
/// [`srgb_intensity`] does on a display that cannot honour it. Offering a
/// schedule that could never come on would be worse than not offering one.
fn night_light_schedule(display: &'static str, night: NightLight) -> Entry {
    let at = crate::sun::location();
    let sun = sun_today();
    let mut values = vec![
        value(
            Schedule::AllDay.title(),
            Some("On for as long as the switch above is"),
            night.schedule == Schedule::AllDay,
            setting(display, DisplayValue::NightLightSchedule(Schedule::AllDay)),
        ),
        value(
            Schedule::Hours.title(),
            Some("Between two hours of your own, set on the page behind this one"),
            night.schedule == Schedule::Hours,
            setting(display, DisplayValue::NightLightSchedule(Schedule::Hours)),
        ),
    ];
    // Second in the list, between the two: it is the middle answer — a
    // schedule, but not one anybody has to set.
    let sun_row = match &at {
        Some(at) => value(
            Schedule::SunsetToSunrise.title(),
            Some(&match sun {
                Some(crate::sun::Sun::Daily { sunrise, sunset }) => format!(
                    "{} to {} today, at {}",
                    clock_title(sunset),
                    clock_title(sunrise),
                    at.name
                ),
                Some(crate::sun::Sun::NeverRises) => {
                    format!("The sun does not rise at {} today", at.name)
                }
                Some(crate::sun::Sun::NeverSets) => {
                    format!("The sun does not set at {} today", at.name)
                }
                None => format!("Worked out for {}", at.name),
            }),
            night.schedule == Schedule::SunsetToSunrise,
            setting(
                display,
                DisplayValue::NightLightSchedule(Schedule::SunsetToSunrise),
            ),
        ),
        None => reading(
            Schedule::SunsetToSunrise.title(),
            "This machine's time zone names no place, so there is no sunset here to follow",
        ),
    };
    values.insert(1, sun_row);

    folder(
        "Schedule",
        &schedule_of(night, sun),
        icons::SETTING_SCHEDULE,
        values,
    )
}

/// The hour it comes on at.
///
/// Twenty-four rows and no more: whether hours are kept at all is the row
/// above, so this one has only to say which. It is only built where they are
/// being kept — see [`night_light_controls`].
fn night_light_from(display: &'static str, night: NightLight) -> Entry {
    folder(
        "From",
        &format!("Comes on at {}", hour_title(night.from)),
        icons::SETTING_SCHEDULE,
        HOURS
            .iter()
            .map(|hour| {
                value(
                    &hour_title(*hour),
                    None,
                    night.from == *hour,
                    setting(display, DisplayValue::NightLightFrom(*hour)),
                )
            })
            .collect(),
    )
}

/// The hour it goes off again.
///
/// Twenty-three rows, not twenty-four: the hour it starts at is not one of
/// them, because a window that ends where it begins is either a whole day or
/// none of one and there is no way to look at the row and tell which.
fn night_light_until(display: &'static str, night: NightLight) -> Entry {
    folder(
        "Until",
        &format!("Goes off at {}", hour_title(night.until)),
        icons::SETTING_SCHEDULE,
        HOURS
            .iter()
            .filter(|hour| **hour != night.from)
            .map(|hour| {
                value(
                    &hour_title(*hour),
                    Some(&window_length(night.from, *hour)),
                    night.until == *hour,
                    setting(display, DisplayValue::NightLightUntil(*hour)),
                )
            })
            .collect(),
    )
}

/// The hours of a day, which both schedule rows are lists of.
const HOURS: [u8; 24] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23,
];

/// An hour as a row is titled with it.
fn hour_title(hour: u8) -> String {
    clock_title(hour.min(23) as u16 * 60)
}

/// How long a window lasts, for the line under an ending hour.
///
/// The one thing the two hours do not say between them, because the arithmetic
/// wraps: 21:00 to 07:00 is ten hours and 07:00 to 21:00 is fourteen, and the
/// pair of numbers looks much the same either way round.
fn window_length(from: u8, until: u8) -> String {
    let hours = (24 + until as i16 - from as i16) % 24;
    match hours {
        1 => "One hour of night light".to_string(),
        hours => format!("{hours} hours of night light"),
    }
}

/// High dynamic range: one subcategory per screen that can do it — unless
/// there is only one, in which case its settings stand here directly.
///
/// The screen comes before the settings because the settings *are* per screen,
/// and a page that offered one set of them for a session with an HDR
/// television beside an SDR laptop panel would be describing a machine nobody
/// has. Screens appear here on the compositor's word and disappear when they
/// are unplugged; nothing has to be configured for a new one to show up, and
/// nothing anywhere in the shell knows the name of any particular one.
///
/// Screens that cannot do HDR are left out rather than listed and greyed. What
/// they would offer is nothing, and a list of everything plugged in with most
/// of it inert is a worse answer than a short list that is all live.
///
/// That leaves three shapes for this column, and all three are wanted: no
/// screen, one screen, or several.
fn high_dynamic_range() -> Entry {
    let capable: Vec<(String, Support)> = support()
        .into_iter()
        .filter(|(_, support)| support.available)
        .collect();

    match capable.as_slice() {
        // Never empty: the bar refuses to step into a column with nothing in
        // it, so a session with no HDR display would have a row that silently
        // did nothing when pressed. What it needs to say is *why*, and this is
        // the only place left to say it.
        [] => folder(
            "HDR",
            "High dynamic range",
            icons::SETTING_HDR,
            vec![nothing_supports_hdr()],
        ),
        // One screen, so there is nothing to choose between: asking which
        // display to configure when there is only one is a question with a
        // single answer, and a step that exists only to be walked through
        // reads as though something else were on offer. The settings take its
        // place, and the screen is named in the row above them so it is still
        // clear what they belong to.
        [(name, support)] => folder(
            "HDR",
            &format!("{name} — {}", state_of(*support)),
            icons::SETTING_HDR,
            controls(name, *support),
        ),
        _ => folder(
            "HDR",
            "High dynamic range",
            icons::SETTING_HDR,
            capable
                .iter()
                .map(|(name, support)| screen(name, *support))
                .collect(),
        ),
    }
}

/// One screen's row in the list, opening onto its settings.
fn screen(name: &str, support: Support) -> Entry {
    folder(
        name,
        &state_of(support),
        icons::SETTING_DISPLAY,
        controls(name, support),
    )
}

/// The four controls, for one screen.
///
/// Shared by the screen list and by the session that has only one screen, so
/// the page is the same page either way — the level above it is what differs.
fn controls(name: &str, support: Support) -> Vec<Entry> {
    let display = intern(name);
    let settings = hdr_for(display);
    vec![
        hdr_switch(display, settings),
        sdr_brightness(display, settings),
        srgb_intensity(display, settings, support),
        peak_brightness(display, settings, support),
    ]
}

/// What a screen is doing, in the few words a row's comment has space for.
fn state_of(support: Support) -> String {
    let peak = match support.peak {
        0 => String::new(),
        peak => format!(", peak {peak} cd/m²"),
    };
    let state = if support.active { "In HDR" } else { "Ready" };
    format!("{state}{peak}")
}

/// The row that stands in for the screen list when there is no screen to list.
fn nothing_supports_hdr() -> Entry {
    reading(
        "No display supports HDR",
        "No connected display reports HDR, or the driver has no colour \
         pipeline to feed it",
    )
}

fn hdr_switch(display: &'static str, settings: Hdr) -> Entry {
    let on = settings.enabled;
    folder(
        "HDR",
        "Send the picture as BT.2020 and PQ",
        icons::SETTING_HDR,
        vec![
            value("Off", None, !on, setting(display, DisplayValue::Hdr(false))),
            value("On", None, on, setting(display, DisplayValue::Hdr(true))),
        ],
    )
}

/// The luminance white is sent at.
///
/// The single most consequential control on the page, because nothing LineXinBar
/// composites is HDR content: every pixel of every application, and of the
/// shell itself, is SDR, so this alone decides whether an HDR session comes out
/// dim, right, or painful. sRGB's own reference is 80 cd/m², which is a darkened
/// grading suite; a screen in a lit room wants a good deal more than that.
fn sdr_brightness(display: &'static str, settings: Hdr) -> Entry {
    let in_force = settings.sdr_brightness;
    folder(
        "SDR brightness",
        "How bright plain white is",
        icons::BRIGHTNESS,
        SDR_BRIGHTNESS
            .iter()
            .map(|(nits, note)| {
                value(
                    &format!("{nits} cd/m²"),
                    Some(note),
                    *nits == in_force,
                    setting(display, DisplayValue::SdrBrightness(*nits)),
                )
            })
            .collect(),
    )
}

const SDR_BRIGHTNESS: &[(u16, &str)] = &[
    (80, "The sRGB reference: a darkened room"),
    (100, "A dim room"),
    (120, "A dim room, a little brighter"),
    (150, "An ordinary lit room"),
    (200, "An ordinary lit room, and the default"),
    (250, "A bright room"),
    (300, "A very bright room"),
    (400, "Daylight on the screen"),
];

/// How far sRGB's colours are stretched on their way into BT.2020.
///
/// The setting nobody else offers and everybody wants. An HDR signal carries
/// BT.2020's primaries, which are far wider than sRGB's, and there are two
/// honest things to do with an sRGB picture inside them: place its colours
/// where they actually belong, so it looks exactly as it did in SDR, or send
/// its numbers through untouched, so its red is displayed as BT.2020's red and
/// everything comes out enormously more saturated. Neither is wrong. This is
/// the blend between them.
///
/// It is also the one control here that some hardware cannot honour, so it is
/// the one that has to be able to say so. It is the CRTC's colour matrix, and
/// a matrix is only a gamut conversion when it acts on linear light — so it
/// needs a degamma stage in front of it, which a good deal of hardware does
/// not have. Where the compositor reports none, the choice is replaced by the
/// reason there is no choice.
fn srgb_intensity(display: &'static str, settings: Hdr, support: Support) -> Entry {
    if !support.gamut {
        return folder(
            "sRGB color intensity",
            "Not available on this display",
            icons::SETTING_APPEARANCE,
            // One line. A row's comment is drawn at a fixed height and the
            // second line of a long one is cut off, so the whole of the
            // explanation cannot live here — the rest of it is in the manual.
            vec![reading(
                "Fixed at its most saturated",
                "No degamma stage in this driver; brightness is unaffected",
            )],
        );
    }

    let in_force = settings.srgb_intensity;
    folder(
        "sRGB color intensity",
        "How saturated sRGB colour is made",
        icons::SETTING_APPEARANCE,
        SRGB_INTENSITY
            .iter()
            .map(|(percent, note)| {
                value(
                    &format!("{percent}%"),
                    Some(note),
                    *percent == in_force,
                    setting(display, DisplayValue::SrgbIntensity(*percent)),
                )
            })
            .collect(),
    )
}

const SRGB_INTENSITY: &[(u8, &str)] = &[
    (0, "Exactly the colour SDR showed"),
    (25, "A little more saturated than SDR"),
    (50, "Half way to BT.2020's own primaries"),
    (75, "Strongly saturated"),
    (100, "sRGB's primaries sent as BT.2020's: vivid"),
];

/// The peak declared to the display, which is what it tone-maps against.
fn peak_brightness(display: &'static str, settings: Hdr, support: Support) -> Entry {
    let in_force = settings.peak_brightness;
    let mut values = vec![value(
        "Display default",
        Some(&match support.peak {
            0 => "Whatever the display says it can do".to_string(),
            peak => format!("What this display reports: {peak} cd/m²"),
        }),
        in_force == 0,
        setting(display, DisplayValue::PeakBrightness(0)),
    )];
    values.extend(PEAK_BRIGHTNESS.iter().map(|(nits, note)| {
        value(
            &format!("{nits} cd/m²"),
            Some(note),
            *nits == in_force,
            setting(display, DisplayValue::PeakBrightness(*nits)),
        )
    }));
    folder(
        "Peak brightness",
        "The brightest the display is told to expect",
        icons::BRIGHTNESS,
        values,
    )
}

const PEAK_BRIGHTNESS: &[(u16, &str)] = &[
    (400, "An entry-level HDR display"),
    (600, "A mid-range HDR display"),
    (1000, "The level most HDR content is graded for"),
    (1400, "A bright HDR display"),
    (4000, "A reference mastering monitor"),
];

/// Sound: where the machine's goes and comes from, and what the shell itself
/// plays.
///
/// Two kinds of thing under one row, and the comment has to own that. The
/// devices are the *machine's* — every application on it plays through the one
/// chosen here, and the choice outlives this shell — while the music is
/// LineXinBar's own, as everything under Appearance is. They belong together all
/// the same: a user who has come looking for anything about sound has come
/// looking for this row, and a console that hid the output device somewhere
/// else because of who owns it would be arranged around its own internals.
///
/// The devices come first because they are what a session is set up with, and
/// because the output is the one row here somebody arrives at a new machine
/// needing: nothing else on this page can be heard until the sound is coming
/// out of the right place.
///
/// Not how loud any of it is. That is the mixer's System row in the guide, which
/// is where a level belongs — beside the volume bar the user is already holding
/// a direction on, and previewing itself at every step of it. A column of the
/// bar cannot preview a level without becoming a volume bar with extra steps,
/// and the shell already has one. What is left for this page is the questions a
/// bar cannot answer: which device, and what plays at all.
///
/// The speaker is the volume bar's own glyph rather than a second drawing of one
/// made for this row, and the output below it wears the same one again: the same
/// object, drawn once, under the lamp every glyph in the shell is under.
/// Settings > Display > HDR already does this with the brightness glyph on two
/// of its rows, and with its own on the switch inside it. A second speaker drawn
/// for this page could only be the same speaker again or a worse one, and the
/// shell would then have two of them to keep in step through every retheme. The
/// input is the row that cannot borrow — there is no microphone anywhere else in
/// the shell, and a microphone drawn as a speaker would be saying the wrong
/// thing rather than repeating a right one — so that one is drawn. See
/// [`icons::SETTING_MICROPHONE`].
fn sounds() -> Entry {
    folder(
        "Sounds",
        "The machine's sound, and the shell's own",
        icons::VOLUME,
        vec![
            device_page(Direction::Output),
            device_page(Direction::Input),
            start_music_switch(),
        ],
    )
}

/// Everything the machine can play through, or everything it can record from,
/// with the one it is using marked.
///
/// One function for both directions, because they are one page asked twice:
/// the same list, the same mark, the same sentence about what choosing does.
/// Writing them separately would be writing the second one *nearly* the same,
/// which is how the input page ends up explaining itself in different words
/// from the output page above it.
///
/// The row above the list says which device is in force, the way the
/// single-screen Resolution and HDR pages name the screen and what it is doing.
/// It is the answer to the question the user came with — *what is it playing
/// through?* — and having it there means the common case is answered without
/// stepping in at all.
fn device_page(direction: Direction) -> Entry {
    let listed = DEVICES.lock().unwrap();
    let devices = listed.of(direction);
    let (title, icon) = match direction {
        Direction::Output => ("Output device", icons::VOLUME),
        Direction::Input => ("Input device", icons::SETTING_MICROPHONE),
    };
    let comment = match devices.iter().find(|device| device.default) {
        Some(device) => match &device.profile {
            Some(profile) => format!("{} — {profile}", device.title),
            None => device.title.clone(),
        },
        // Either there is nothing to name, or the machine is using something
        // this page does not list — a monitor of an output, say, chosen as the
        // input somewhere else. Both are answered by what is inside rather than
        // by a comment that would have to guess.
        None => match direction {
            Direction::Output => "Where everything on the machine plays".to_string(),
            Direction::Input => "What everything on the machine records from".to_string(),
        },
    };
    // Never an empty column: the bar refuses to step into one, so a machine
    // with no devices would have a row that silently did nothing when pressed.
    // What it has to say instead is *why* there is nothing to choose.
    let rows = match devices {
        [] => vec![nothing_to_choose(direction, listed.server)],
        devices => devices
            .iter()
            .map(|device| {
                value(
                    &device.title,
                    device.profile.as_deref(),
                    device.default,
                    Setting::SoundDevice {
                        direction,
                        id: intern(&device.id),
                    },
                )
            })
            .collect(),
    };
    folder(title, &comment, icon, rows)
}

/// The row that stands in for the device list when there is no device to list.
///
/// The two cases are not the same fact and must not read as one. A sound server
/// that lists nothing is a machine with no sound card in it, or one whose card
/// has no profile that can play; no sound server at all is a session where
/// nothing is in charge of the question — every program opens ALSA and takes
/// whatever the kernel gives it, and there is no *machine's* device for this
/// page to set. The first is about the hardware, the second about the session,
/// and a user is entitled to know which they are looking at.
fn nothing_to_choose(direction: Direction, server: bool) -> Entry {
    let thing = match direction {
        Direction::Output => "output",
        Direction::Input => "input",
    };
    if !server {
        return reading(
            "No sound server is running",
            "Without PipeWire or PulseAudio nothing decides this for the \
             machine; each program opens the sound card itself",
        );
    }
    reading(
        &format!("No {thing} device"),
        &format!("The sound server lists no {thing} on this machine"),
    )
}

/// The Start screen's background music, on or off.
///
/// Off and On in that order and marked the way every other switch in this tree
/// is, because it is the same kind of question as the HDR one and a shell with
/// two shapes of switch in it would be a shell where the second one has to be
/// read before it can be used.
///
/// The music is a property of the *session* rather than of a screen — it plays
/// while every display is showing Start and nothing at all is open — so unlike
/// everything under Display this row names no screen and there is one of it.
/// See [`crate::sound`], which turns this into silence in the same breath as a
/// muted mixer: the stream is dropped rather than left advancing where nobody
/// can hear it, and turning it back on begins the track again from its
/// beginning.
///
/// The note is the Music shelf's glyph, the way the speaker above it is the
/// volume bar's. It is read here inside Sounds, where there is no library beside
/// it to be mistaken for: what a note means in a column of the user's own files
/// is that column's contents, and what it means under a speaker is music.
fn start_music_switch() -> Entry {
    let on = start_music();
    folder(
        "Start music",
        "The music the start screen plays",
        icons::CATEGORY_MUSIC,
        vec![
            value("Off", None, !on, Setting::StartMusic(false)),
            value("On", None, on, Setting::StartMusic(true)),
        ],
    )
}

/// Network: what this machine is on.
///
/// The third of the three things a session does with the world outside itself —
/// the picture goes out, the sound goes out, and this goes both ways — so it
/// stands with Display and Sounds rather than under System, and after them
/// because it is the one of the three a console can be used without.
///
/// What is inside depends on what is in the machine, all the way down. A
/// desktop with no radio in it has no Wi-Fi page; a machine with no socket has
/// no Wired page; one with neither has a row saying so. That is the same rule
/// the HDR page is built by — a screen that cannot do it is not listed — and it
/// matters more here, because a Wi-Fi page on a machine with no wireless card
/// is not merely useless: it is a page that would have the user turning a radio
/// on and off looking for a network that was never going to appear.
///
/// See [`crate::network`] for why this one page in the tree depends on a daemon
/// when nothing else does.
fn network() -> Entry {
    let listing = network_listing();
    let radios = listing.of(crate::network::Kind::Wireless);
    let sockets = listing.of(crate::network::Kind::Wired);
    let mut rows = Vec::new();
    if !sockets.is_empty() {
        rows.push(wired(&sockets));
    }
    if !radios.is_empty() {
        rows.push(wireless(&listing, &radios));
    }
    if rows.is_empty() {
        // Never an empty column: the bar refuses to step into one, so a machine
        // with nothing to configure would have a row that silently did nothing
        // when pressed. What it has to say instead is *why* — and the two
        // reasons are not the same fact. See [`nothing_to_connect_with`].
        rows.push(nothing_to_connect_with(listing.manager));
    }
    folder(
        "Network",
        "The socket in the back of the machine, and the air around it",
        icons::SETTING_NETWORK,
        rows,
    )
}

/// The row that stands in for the pages when there is nothing to put on them.
///
/// The two cases are not the same fact and must not read as one, exactly as the
/// sound device page's two are not: a machine whose network manager lists
/// nothing has no network hardware in it, and a session with no network manager
/// is one where nothing is in charge of the question — the machine may well be
/// on a network, configured by something this shell cannot see. The first is
/// about the hardware, the second about the session, and a user is entitled to
/// know which they are looking at.
fn nothing_to_connect_with(manager: bool) -> Entry {
    if !manager {
        return reading(
            "No network manager is running",
            "Without NetworkManager nothing here decides what this machine is \
             on; whatever configured the network did so outside this session",
        );
    }
    reading(
        "No network hardware",
        "The network manager lists neither a wired socket nor a wireless radio \
         on this machine",
    )
}

/// Wi-Fi: the radio, the networks it can hear, and what it is on.
///
/// The three shapes [`resolution`] has, for the same reason — except that the
/// radio switch stands above the lot of them rather than inside each. It is one
/// switch for the whole machine, whatever is in it: `NetworkManager` has a
/// single `WirelessEnabled`, and a page that offered one per card would be
/// offering a control that does not exist.
fn wireless(listing: &crate::network::Listing, radios: &[&crate::network::Device]) -> Entry {
    let mut rows = vec![radio_switch(listing)];
    let note = match radios {
        // One radio, so there is no radio to choose between: its controls take
        // the place of the list, and the row above them says what it is doing.
        [only] => {
            rows.extend(wireless_controls(listing, only));
            device_note(only)
        }
        many => {
            rows.extend(many.iter().map(|radio| {
                folder(
                    &radio.interface,
                    &device_note(radio),
                    icons::SETTING_WIFI,
                    wireless_controls(listing, radio),
                )
            }));
            "The wireless radios in this machine".to_string()
        }
    };
    folder("Wi-Fi", &note, icons::SETTING_WIFI, rows)
}

/// The radio switch, or the row that says why there is not one.
///
/// A switch the machine will not honour is not a switch. A laptop with its
/// wireless killed by the key above the keyboard reports the radio as
/// unavailable in *hardware*, and offering On there would be offering something
/// that cannot happen — the press would be accepted, nothing would change, and
/// the mark would sit on a row describing a machine that is not this one.
fn radio_switch(listing: &crate::network::Listing) -> Entry {
    if !listing.radio_switchable {
        return reading(
            "Wi-Fi is off at the machine",
            "A switch on this machine has the wireless radio off; nothing in \
             software can turn it back on",
        );
    }
    let on = listing.radio;
    folder(
        "Wi-Fi",
        "Whether the wireless radio is on",
        icons::SETTING_WIFI,
        vec![
            value(
                "Off",
                None,
                !on,
                Setting::Network(NetworkValue::Radio(false)),
            ),
            value("On", None, on, Setting::Network(NetworkValue::Radio(true))),
        ],
    )
}

/// What one radio offers: the networks it can hear, and what it was given.
///
/// Nothing at all when the radio is off, and that is the whole of what being
/// off means on this page: the switch above is the one row left, which is the
/// honest picture of a card that cannot hear anything.
///
/// A radio switched off by a key on the machine is off in exactly the same
/// sense, and gets exactly the same page. It is asked separately because the
/// two are separate facts: `WirelessEnabled` can read true on a card whose
/// hardware switch has it off, and a Networks page built from that one would be
/// a page that never has anything on it and never says why.
///
/// The addressing is not here, and that is the one difference from a socket's
/// page. An address belongs to a *profile*, and a radio's profile is whichever
/// network it is on — so a card that has been on four networks this week has
/// four answers to "what address does this take", and a row here would silently
/// be about one of them. It lives under the network instead; see
/// [`network_row`].
fn wireless_controls(
    listing: &crate::network::Listing,
    radio: &crate::network::Device,
) -> Vec<Entry> {
    if !listing.radio || !listing.radio_switchable {
        return Vec::new();
    }
    let mut rows = vec![networks_page(listing, radio)];
    rows.extend(connection_information(radio));
    rows
}

/// Every network in the air, with the one this radio is on marked.
///
/// The networks and nothing else. This column used to open with a `Not
/// connected` row — an answer to the question the list asks, for a machine that
/// is on none of them, and for a while the only way off a network short of
/// turning the whole radio off. That reason is gone: the network the radio is
/// on is stepped into now, and `Disconnect` is in there where somebody looking
/// for a way off this network would look for it. What was left was a row
/// standing above the list doing the same thing at a distance, which is one row
/// of ceremony on every visit to the page for a press most people make once.
///
/// So on a machine that is on none of them, nothing here is marked, and that is
/// the honest picture: the question has no answer yet rather than an answer
/// called none. The column then opens on its first row, which is the strongest
/// network in the air — which is what somebody who came here to get connected
/// was reaching for.
///
/// A radio that hears nothing gets a line saying so rather than an empty
/// column. `Networks` says `Nothing in range` before it is stepped into, but a
/// row that opens onto nothing at all reads as a shell that failed rather than
/// as an answer — and [`crate::model::Cursor::enter`] will not open an empty
/// column, so the press would do nothing whatever.
fn networks_page(listing: &crate::network::Listing, radio: &crate::network::Device) -> Entry {
    let device = intern(&radio.path);
    let networks = listing.networks_of(&radio.path);
    let rows = match networks.is_empty() {
        true => vec![reading(
            "Nothing in range",
            "The radio is on and hearing nothing. Networks appear here as they \
             are found.",
        )],
        false => networks
            .iter()
            .map(|network| network_row(device, radio, network))
            .collect(),
    };
    folder(
        "Networks",
        &networks_note(radio, networks),
        icons::SETTING_WIFI,
        rows,
    )
}

/// What the Networks row says before it is stepped into.
fn networks_note(radio: &crate::network::Device, networks: &[crate::network::Network]) -> String {
    if let Some(trouble) = radio.trouble.as_deref() {
        return trouble.to_string();
    }
    if let Some(joined) = networks.iter().find(|network| network.joined) {
        return format!("{} — {}%", joined.ssid, joined.strength);
    }
    match networks.len() {
        0 => "Nothing in range".to_string(),
        1 => "1 network in range".to_string(),
        many => format!("{many} networks in range"),
    }
}

/// One network's row.
///
/// Four shapes, and which one a network gets says what pressing it does.
///
/// An enterprise network is a [`reading`] rather than a choice, and that is the
/// one place on this page where a row the user can see is a row they cannot
/// press. It is listed at all because leaving it out would answer "my network is
/// not here" with silence; it carries no setting because joining it needs a user
/// name, a certificate and a server's agreement, and a shell that took a
/// password for it would be collecting something nobody asked for.
///
/// The one the radio is *on* is a subcategory carrying the tick — see
/// [`crate::apps::Folder::chosen`] — because it is the only row here whose
/// press has nothing to do. Joining a network the radio is already on is a
/// no-op, and behind that press is the one thing this page could not otherwise
/// say: the address and the name servers this machine takes, which belong to
/// the profile it is connected by and to no other. So the tick still says which
/// network is in force, the column still opens on it, and stepping in is where
/// the addressing lives.
///
/// A network this machine has a *profile* for is a subcategory too, without the
/// tick, and for a reason of the same kind: a saved network is the one other
/// row here that has more than one thing to do. It can be joined, and it can be
/// forgotten — and forgetting is not something a list of networks can offer
/// anywhere else, because there is nowhere else in this shell that knows which
/// networks the machine remembers. Pressing it no longer joins on the spot; see
/// [`saved_network`], which says what that costs and why it is worth it.
///
/// Every other network is a value: pressing it joins. That is the whole of a
/// network the machine has never been on — there is nothing saved to remove and
/// nothing to configure until it has been joined once — so it keeps the single
/// press, which is what a user picking their own network out of the air wants.
///
/// Nothing here is marked on a machine that is on none of them, and that is
/// deliberate; see [`networks_page`].
fn network_row(
    device: &'static str,
    radio: &crate::network::Device,
    network: &crate::network::Network,
) -> Entry {
    use crate::network::Security;
    if network.security == Security::Enterprise {
        return reading(
            &network.ssid,
            &format!(
                "{}% — this network needs a user name and a certificate, which \
                 have to be set up outside this shell",
                network.strength
            ),
        );
    }
    // Whether or not a profile can be read for the device. It used to want one,
    // because the only thing behind this row was the addressing and there is
    // none without a profile; now the way off the network is in there too, and
    // that must not depend on `NetworkManager` having got round to publishing an
    // active connection for a device whose radio is already associated. See
    // [`ip_address`], which says so in words where there is nothing to
    // configure yet.
    if network.joined {
        return joined_network(device, radio, network);
    }
    if network.saved {
        return saved_network(device, network);
    }
    value(
        &network.ssid,
        Some(&network_note_of(network)),
        network.joined,
        Setting::Network(NetworkValue::Join {
            device,
            ssid: intern(&network.ssid),
        }),
    )
}

/// The network the radio is on: its addressing, and the two ways off it.
///
/// The addressing stands above the pair that act, and the order is the whole of
/// the protection this page gives them. A column opens on the row in force, and
/// nothing here is in force — so it opens on the first row, and the first row
/// has to be one that does nothing but open another column. Somebody stepping
/// into their own network and pressing Accept twice out of habit lands on `IP
/// address`, not on Forget.
///
/// Between the two that act, Disconnect stands first for the reason Off stands
/// above On in every switch in this tree: it is the one that does less, and it
/// is the one that can be undone from the page it leaves behind.
///
/// Disconnect is the only way off a network in the shell, which is why it is
/// built for every joined row and not only for one with a profile behind it.
/// The column this row stands in used to carry a `Not connected` answer that
/// did the same thing from outside; it does not any more, because a way off
/// *this* network belongs on the page about this network, where somebody
/// looking for it will look.
fn joined_network(
    device: &'static str,
    radio: &crate::network::Device,
    network: &crate::network::Network,
) -> Entry {
    let ssid = intern(&network.ssid);
    let mut rows = vec![ip_address(radio, &network.ssid)];
    rows.extend(dns_page(radio, &network.ssid));
    rows.push(action(
        "Disconnect",
        "Come off this network, and keep it saved",
        icons::SETTING_DISCONNECT,
        Setting::Network(NetworkValue::Leave { device }),
    ));
    rows.push(forget(device, ssid));
    chosen_folder(
        &network.ssid,
        &network_note_of(network),
        icons::SETTING_WIFI,
        rows,
    )
}

/// A network this machine remembers but is not on: joining it again, and
/// forgetting it.
///
/// This costs a press. Joining a saved network used to be one Accept on the
/// list and is now two, and that is worth saying plainly because it is the one
/// change on this page that takes something away. What it buys is the only
/// place in the shell where a saved network can be removed: the list is
/// otherwise a list of *networks in the air*, where what the machine remembers
/// is invisible except as one word in a comment, and a user who typed the wrong
/// password once has no way to make this shell ask again.
///
/// Connect stands first, so the column opens on it — that is what stepping into
/// a network the machine already knows is for, and it keeps the common press to
/// Accept, Accept from the list.
///
/// A network with nothing saved keeps its single press; see [`network_row`].
fn saved_network(device: &'static str, network: &crate::network::Network) -> Entry {
    let ssid = intern(&network.ssid);
    let rows = vec![
        action(
            "Connect",
            "Join this network again",
            icons::SETTING_CONNECT,
            Setting::Network(NetworkValue::Join { device, ssid }),
        ),
        forget(device, ssid),
    ];
    folder(
        &network.ssid,
        &network_note_of(network),
        icons::SETTING_WIFI,
        rows,
    )
}

/// The row that removes a saved network from the machine.
///
/// One row written once, because the two pages it appears on must not drift:
/// what forgetting costs is the same whether the radio is on the network or
/// not, and it is a sentence a user has one chance to read.
///
/// The comment says what goes rather than what happens. "This machine will ask
/// for the password again" is the consequence somebody needs before they press
/// it — not that a profile is deleted, which is true and means nothing to
/// anyone who has not read `NetworkManager`'s manual.
///
/// It wears the waste bin the context menu's Uninstall row wears, which is the
/// only mark in the shell used from two places. See [`icons::UNINSTALL`]: the
/// bin means *this is taken off the machine*, and that is exactly as true of a
/// saved network as it is of a program.
fn forget(device: &'static str, ssid: &'static str) -> Entry {
    action(
        "Forget",
        "Remove this network: the password will be asked for again",
        icons::UNINSTALL,
        Setting::Network(NetworkValue::Forget { device, ssid }),
    )
}

/// What one network's row says under its name: what joining it takes, how well
/// it is heard, and which band it was heard on.
///
/// "Saved" rather than the protection where there is a profile for it, because
/// those are the same fact seen from either side and only one of them is what
/// the user is about to find out: a saved network joins on the press, and an
/// unsaved secured one asks for a password first.
fn network_note_of(network: &crate::network::Network) -> String {
    let what = if network.joined {
        "Connected"
    } else if network.saved {
        "Saved"
    } else {
        network.security.title()
    };
    match band_of(network.frequency) {
        Some(band) => format!("{what} — {}% at {band}", network.strength),
        None => format!("{what} — {}%", network.strength),
    }
}

/// Which band a frequency in MHz is in, as a person names it.
///
/// The three that are in the air, and nothing for a number in none of them:
/// a row saying "5745 MHz" would be the shell reading a register out loud.
fn band_of(frequency: u32) -> Option<&'static str> {
    match frequency {
        2401..=2495 => Some("2.4 GHz"),
        5150..=5895 => Some("5 GHz"),
        5925..=7125 => Some("6 GHz"),
        _ => None,
    }
}

/// What address this interface takes: asked of the network, or pinned.
///
/// The two values it can be pinned to are inside this column rather than beside
/// it, and that is the one thing about the shape worth arguing over. They could
/// have been rows of the page above — the night light's hours are — but that
/// page would then carry five rows about addressing where it now carries two,
/// and two of the five would be named `Address` under a row named `IP address`.
/// Inside, the column reads as what it is: here are the two answers, and here is
/// what the second one is set to.
///
/// IPv4 only. See [`crate::network::Ipv4`], which says why, and what a machine
/// given a static address still gets over IPv6.
///
/// `whose` is the connection this addressing belongs to, by the name its owner
/// would use for it — a network's own name on a radio, a profile's on a socket.
/// It is not on the rows, which stand under a trail that already says it; it is
/// carried to the panels the typed rows open, which cover that trail. See
/// [`crate::apps::Typed::whose`].
fn ip_address(device: &crate::network::Device, whose: &str) -> Entry {
    let name = intern(&device.path);
    let ipv4 = &device.ipv4;
    if device.profile.is_none() {
        return folder(
            "IP address",
            "Nothing to configure yet",
            icons::SETTING_ADDRESS,
            vec![reading(
                "No connection to configure",
                "An address belongs to a saved connection rather than to the \
                 socket; connect this interface once and it can be set here",
            )],
        );
    }
    let mut rows = vec![value(
        "Automatic",
        Some("Asked for from the network"),
        ipv4.automatic,
        Setting::Network(NetworkValue::Addressing {
            device: name,
            automatic: true,
        }),
    )];
    // Manual needs an address to pin, and where there is none the row says so
    // instead of being a press that is accepted and does nothing — see
    // [`crate::network::Ipv4::can_pin`].
    rows.push(match ipv4.can_pin {
        true => value(
            "Manual",
            Some("Pinned, and kept across reboots"),
            !ipv4.automatic,
            Setting::Network(NetworkValue::Addressing {
                device: name,
                automatic: false,
            }),
        ),
        false => reading(
            "Manual",
            "There is no address to pin yet: connect this interface, and the \
             one it is given can be kept",
        ),
    });
    if !ipv4.automatic {
        rows.push(typed(
            name,
            crate::network::Field::Address,
            ipv4.address.as_deref(),
            whose,
        ));
        rows.push(typed(
            name,
            crate::network::Field::Router,
            ipv4.gateway.as_deref(),
            whose,
        ));
    }
    folder("IP address", &ip_note(ipv4), icons::SETTING_ADDRESS, rows)
}

/// What the row above the addressing says, so the usual question is answered
/// without stepping in.
fn ip_note(ipv4: &crate::network::Ipv4) -> String {
    match (ipv4.automatic, ipv4.address.as_deref()) {
        (true, _) => "Automatic — asked for from the network".to_string(),
        (false, Some(address)) => format!("Manual — {address}"),
        // A manual profile always has an address; this is the moment between
        // the method being written and the address arriving, and it is worth a
        // word rather than an empty line.
        (false, None) => "Manual — no address set".to_string(),
    }
}

/// Where this interface's name servers come from.
///
/// Called DNS on screen and nowhere else in this module, because that is the
/// word every router's own page uses and the one somebody looking for this
/// setting will look for. See [`crate::network::Field::Dns`].
///
/// The choice is offered only under automatic addressing, and that is not a
/// simplification: a pinned profile runs no DHCP, so there is nothing for its
/// name servers to be automatic *from*. Under Manual the column is the list and
/// the reason there is no choice, which is the honest page — offering an
/// "Automatic" there that quietly meant "none at all" would be the one row in
/// this tree that says something untrue.
///
/// `whose` is what it is for [`ip_address`].
fn dns_page(device: &crate::network::Device, whose: &str) -> Option<Entry> {
    let name = intern(&device.path);
    device.profile.as_ref()?;
    let ipv4 = &device.ipv4;
    let mut rows = Vec::new();
    if ipv4.automatic {
        rows.push(value(
            "Automatic",
            Some("Whatever the network offers"),
            ipv4.dns_automatic,
            Setting::Network(NetworkValue::Dns {
                device: name,
                automatic: true,
            }),
        ));
        rows.push(value(
            "Manual",
            Some("Only the ones named below"),
            !ipv4.dns_automatic,
            Setting::Network(NetworkValue::Dns {
                device: name,
                automatic: false,
            }),
        ));
    } else {
        rows.push(reading(
            "Always manual here",
            "A pinned address runs no DHCP, so there is nothing to take name \
             servers from",
        ));
    }
    if !ipv4.automatic || !ipv4.dns_automatic {
        let named = ipv4.dns.join(", ");
        rows.push(typed(
            name,
            crate::network::Field::Dns,
            (!named.is_empty()).then_some(named.as_str()),
            whose,
        ));
    }
    Some(folder(
        "DNS",
        &dns_note(ipv4),
        icons::SETTING_NAME_SERVER,
        rows,
    ))
}

fn dns_note(ipv4: &crate::network::Ipv4) -> String {
    if ipv4.automatic && ipv4.dns_automatic {
        return "Automatic — whatever the network offers".to_string();
    }
    match ipv4.dns.as_slice() {
        [] => "None set".to_string(),
        named => named.join(", "),
    }
}

/// One row that is typed into rather than chosen.
///
/// The comment is the value itself, which is the one thing a row like this has
/// to say: the title names what it is for and the value is what it is. A value
/// nobody has set says so in words rather than leaving the line blank, because
/// a blank line reads as a row that failed to load.
fn typed(
    device: &'static str,
    field: crate::network::Field,
    value: Option<&str>,
    whose: &str,
) -> Entry {
    Entry::Typed(crate::apps::Typed {
        title: field.title().to_string(),
        whose: whose.to_string(),
        comment: match value {
            Some(value) => value.to_string(),
            None => "Not set".to_string(),
        },
        icon: icons::SETTING_TYPED.to_string(),
        value: value.unwrap_or_default().to_string(),
        about: Typing::Network { device, field },
    })
}

/// Wired: the socket, or one page per socket on a machine with several.
///
/// The same three shapes as [`wireless`], without the switch above them: there
/// is no radio to turn off, and what stands in its place is inside each socket's
/// own page, because bringing a socket up is a thing done to that socket.
fn wired(sockets: &[&crate::network::Device]) -> Entry {
    match sockets {
        [only] => folder(
            "Wired",
            &device_note(only),
            icons::SETTING_ETHERNET,
            wired_controls(only),
        ),
        many => folder(
            "Wired",
            "The sockets in the back of the machine",
            icons::SETTING_ETHERNET,
            many.iter()
                .map(|socket| {
                    folder(
                        &socket.interface,
                        &device_note(socket),
                        icons::SETTING_ETHERNET,
                        wired_controls(socket),
                    )
                })
                .collect(),
        ),
        // `sockets` is never empty: the caller does not build this page when
        // there is nothing to put on it. Answered anyway rather than matched
        // exhaustively away, because a column with no rows in it is the one
        // shape the bar cannot step into.
    }
}

/// What one socket offers.
fn wired_controls(socket: &crate::network::Device) -> Vec<Entry> {
    let whose = whose_connection(socket);
    let mut rows = vec![wired_switch(socket), ip_address(socket, &whose)];
    rows.extend(dns_page(socket, &whose));
    rows.extend(connection_information(socket));
    rows
}

/// What to call the connection an interface's addressing belongs to.
///
/// The profile's own name where there is one — `Wired connection 1`, which is
/// what `NetworkManager` files it under and what every other tool on the
/// machine shows. Failing that the interface, which is the only other name the
/// thing has: a socket with a saved profile it is not running still has an
/// address to set, and `enp8s0` is a poorer subject than a profile name but a
/// far better one than nothing.
///
/// A wireless network does not come through here. Its name is the network's,
/// which the row already has in hand — see [`network_row`].
fn whose_connection(device: &crate::network::Device) -> String {
    device
        .connection
        .clone()
        .unwrap_or_else(|| device.interface.clone())
}

/// Whether a socket is up, as a switch.
///
/// A socket with no cable in it is not offered the switch: there is nothing for
/// On to do, and a row that accepted the press and left the mark on Off would be
/// the page arguing with the user about something they can see by looking at the
/// back of the machine.
fn wired_switch(socket: &crate::network::Device) -> Entry {
    if socket.carrier == Some(false) {
        return reading(
            "No cable",
            "Nothing is plugged into this socket, so there is no connection to \
             turn on",
        );
    }
    let device = intern(&socket.path);
    let up = socket.up();
    folder(
        "Connection",
        "Whether this socket is connected",
        icons::SETTING_ETHERNET,
        vec![
            value(
                "Off",
                None,
                !up,
                Setting::Network(NetworkValue::Wire { device, up: false }),
            ),
            value(
                "On",
                None,
                up,
                Setting::Network(NetworkValue::Wire { device, up: true }),
            ),
        ],
    )
}

/// What a device was given, as a page to read.
///
/// Behind a row rather than on the page above it, and only when there is an
/// address to report. These are facts, not settings: a page of settings with
/// four unpressable rows at the bottom of it is a page whose controls are
/// outnumbered by its footnotes, and a user walking down it has to read past
/// them to find out there is nothing more to change. It is the same division
/// the System page makes with [`system_information`], one level in.
fn connection_information(device: &crate::network::Device) -> Option<Entry> {
    let address = device.address.as_deref()?;
    let mut values = vec![("IP address".to_string(), address.to_string())];
    if let Some(gateway) = device.gateway.as_deref() {
        values.push(("Router".to_string(), gateway.to_string()));
    }
    match device.nameservers.as_slice() {
        [] => {}
        // Every one of them, on one line. A machine is given two or three and
        // they are one answer — which name servers am I using — rather than
        // three separate facts.
        servers => values.push(("DNS".to_string(), servers.join(", "))),
    }
    values.push(("Interface".to_string(), device.interface.clone()));
    if let Some(hardware) = device.hardware.as_deref() {
        values.push(("Hardware address".to_string(), hardware.to_string()));
    }
    if device.speed > 0 {
        values.push(("Link speed".to_string(), format!("{} Mb/s", device.speed)));
    }
    Some(Entry::Facts(crate::apps::Facts {
        title: "Connection information".to_string(),
        comment: device.interface.clone(),
        icon: icons::SETTING_INFO.to_string(),
        about: crate::apps::About::Listed(values),
    }))
}

/// What one interface is doing, in the few words a row's comment has room for.
fn device_note(device: &crate::network::Device) -> String {
    use crate::network::Link;
    if let Some(trouble) = device.trouble.as_deref() {
        return trouble.to_string();
    }
    match device.link {
        Link::Up => match (device.connection.as_deref(), device.speed) {
            (Some(connection), 0) => connection.to_string(),
            (Some(connection), speed) => format!("{connection} — {speed} Mb/s"),
            (None, _) => "Connected".to_string(),
        },
        Link::Working => "Connecting…".to_string(),
        Link::Idle => "Not connected".to_string(),
        Link::Failed => "Could not connect".to_string(),
        Link::Unavailable => "Not ready".to_string(),
    }
}

/// What the Bluetooth row is called, in the one place that decides it.
///
/// Written down rather than spelt twice because something outside this module
/// has to recognise it: opening this page is part of what sets a radio looking
/// around, and the shell asks which page is open by name. See
/// [`crate::bluetooth::Bt::watch`] and [`SEARCH_PAGE`].
pub const BLUETOOTH_PAGE: &str = "Bluetooth";

/// And the one page inside it that costs the machine something to have open.
///
/// The scan runs while this column is open and at no other time — not while the
/// devices are being read, not while Settings is on screen. See
/// [`search_page`], which is where the argument for that is.
pub const SEARCH_PAGE: &str = "Search to pair";

/// Bluetooth: what this machine is paired with.
///
/// Three rows, and they are the three questions somebody arrives here with: is
/// it on, what is it talking to, and what is this machine to everything else.
/// Underneath the first two it is the Wi-Fi page's shape — a switch, and a
/// column of things to connect to with the one it is on marked — because a user
/// who has joined a wireless network on this shell has already learnt how to
/// connect a pair of headphones.
///
/// **There is no page per controller, and that is the point of the third row.**
/// There used to be: a machine with a card on the board and a dongle in the
/// front got a column reading `hci0`, `hci1`, which is the kernel's own name for
/// a thing and says nothing whatever to the person in front of the screen. Which
/// radio the machine uses is a question with one answer, it is asked once, and
/// it belongs with the rest of what this machine *is* — so it lives under
/// Configuration and everything above it is about the one controller in force.
/// See [`chosen_controller`].
///
/// A machine with no controller in it gets one row saying so, rather than the
/// whole column being left out: a Bluetooth row that is simply missing is
/// indistinguishable from a shell that does not do Bluetooth.
///
/// See [`crate::bluetooth`] for why this is the second page in the tree that
/// depends on a daemon, and for the one thing it has that its sister does not:
/// pairing is a conversation, and this session has to be the one that answers.
fn bluetooth() -> Entry {
    let listing = bluetooth_listing();
    let rows = match chosen_controller(&listing) {
        // Never an empty column: the bar refuses to step into one. See
        // [`no_bluetooth`], which is also where the two reasons are told apart.
        None => vec![no_bluetooth(listing.manager)],
        Some(controller) => {
            let mut rows = vec![power_switch(controller)];
            // Nothing to list on a radio that cannot hear anything, which is
            // the whole of what being off means here — the honest picture, and
            // the one [`wireless_controls`] draws. Configuration stays: which
            // radio this machine uses, what it is called and what happens at
            // startup are all questions with answers while it is off.
            if controller.powered && controller.switchable {
                rows.push(devices_page(&listing, controller));
            }
            rows.push(configuration(&listing, controller));
            rows
        }
    };
    folder(
        BLUETOOTH_PAGE,
        "The devices this machine pairs with, and the controller it pairs from",
        icons::SETTING_BLUETOOTH,
        rows,
    )
}

/// Which controller the machine's Bluetooth *is*.
///
/// The one the user chose, if it is still in the machine; failing that the
/// first BlueZ lists. Both halves matter. A dongle that has been unplugged must
/// not leave the page blank — the card on the board is still there and is still
/// Bluetooth — and a machine nobody has chosen on has to work without anybody
/// choosing, which is what every machine with one radio in it is.
///
/// Matched by address rather than by path, for the reason the preference is
/// stored that way: `hci0` is the order the kernel probed them in. See
/// [`BLUETOOTH_CONTROLLER`].
pub fn chosen_controller(
    listing: &crate::bluetooth::Listing,
) -> Option<&crate::bluetooth::Controller> {
    let wanted = bluetooth_controller();
    listing
        .controllers
        .iter()
        .find(|controller| Some(controller.address.as_str()) == wanted.as_deref())
        .or_else(|| listing.controllers.first())
}

/// The row that stands in for the pages when there is no controller to draw
/// them from.
///
/// Both cases say the same thing at the top, because it is the same thing to
/// the user: there is no Bluetooth to be had here. What differs is the line
/// under it, and that difference is worth keeping for the reason
/// [`nothing_to_connect_with`] keeps its own — a machine with no controller in
/// it will never have Bluetooth until somebody plugs one in, and a session
/// whose Bluetooth service is not running is a machine that has it and has
/// nothing in charge of it. The first is about the hardware, the second about
/// the session, and only one of them is worth going to look at.
fn no_bluetooth(manager: bool) -> Entry {
    if !manager {
        return reading(
            "Bluetooth is not available",
            "The Bluetooth service is not running, so nothing in this session \
             is in charge of Bluetooth",
        );
    }
    reading(
        "Bluetooth is not available",
        "There is no Bluetooth controller in this machine, so there is nothing \
         to pair with",
    )
}

/// The switch, or the row that says why there is not one.
///
/// A switch the machine will not honour is not a switch — see [`radio_switch`],
/// which is the same argument about the radio beside it. The laptop key that
/// kills wireless usually kills this too, and the two rows then say so
/// separately because they are two radios.
fn power_switch(controller: &crate::bluetooth::Controller) -> Entry {
    if !controller.switchable {
        return reading(
            "Bluetooth is off at the machine",
            "A switch on this machine has the Bluetooth radio off; nothing in \
             software can turn it back on",
        );
    }
    let path = intern(&controller.path);
    let on = controller.powered;
    folder(
        "Bluetooth",
        "Whether the Bluetooth radio is on",
        icons::SETTING_BLUETOOTH,
        vec![
            value(
                "Off",
                None,
                !on,
                Setting::Bluetooth(BluetoothValue::Power {
                    controller: path,
                    on: false,
                }),
            ),
            value(
                "On",
                None,
                on,
                Setting::Bluetooth(BluetoothValue::Power {
                    controller: path,
                    on: true,
                }),
            ),
        ],
    )
}

/// What this machine is paired with, and the one way to add to it.
///
/// **Only what the machine knows.** Everything in this column is something that
/// has been paired with — connected or not, in the room or not — and the
/// stranger a scan turns up is not here. That is the difference from the
/// wireless page it is otherwise a copy of, and it follows from the difference
/// underneath: a network in the air is a thing to join, and a Bluetooth device
/// in the air is a thing to *pair with*, which is a decision, and a list where
/// the headphones somebody uses every day sit among nine unnamed beacons from
/// the flat upstairs is a list they have to search every time.
///
/// A paired device is listed whether or not it can be heard, which is the other
/// half of the same argument. A network out of range cannot be listed because
/// it is not in the air; headphones in a drawer are a thing the machine is still
/// paired with — and forgetting them is exactly what somebody wants when the
/// device is not to hand.
///
/// So the strangers live one step further in, behind [`search_page`], which is
/// the row above the list rather than below it: it is what somebody arriving
/// with a new thing in their hand is looking for, and it is the only row here
/// that ever costs the radio anything.
fn devices_page(
    listing: &crate::bluetooth::Listing,
    controller: &crate::bluetooth::Controller,
) -> Entry {
    let devices = listing.devices_of(&controller.path);
    let known: Vec<&crate::bluetooth::Device> = devices
        .iter()
        .filter(|device| device.paired || device.connected)
        .collect();
    let mut rows = vec![search_page(devices)];
    rows.extend(known.iter().map(|device| device_row(device)));
    folder(
        "Devices",
        &devices_note(&known),
        icons::SETTING_BLUETOOTH,
        rows,
    )
}

/// What the Devices row says before it is stepped into.
fn devices_note(known: &[&crate::bluetooth::Device]) -> String {
    if let Some(connected) = known.iter().find(|device| device.connected) {
        return connected.name.clone();
    }
    match known.len() {
        0 => "Nothing paired yet".to_string(),
        1 => "1 device paired".to_string(),
        many => format!("{many} devices paired"),
    }
}

/// Everything in the air that this machine has never been paired with.
///
/// **The radio looks around while this column is open and at no other time.**
/// That is the whole reason this is a page rather than a section of the one
/// above it, and it is worth being plain about what it buys. A scanning
/// controller shares its radio with whatever it is already carrying: a pair of
/// headphones playing through the same adapter can be *heard* to mind, in
/// dropouts, and on a laptop it is a measurable amount of battery. Tying the
/// scan to Settings being on screen — which is what this page did before —
/// meant somebody adjusting the night light with music playing paid for a list
/// they were not looking at. Tying it to a row they pressed on purpose means
/// the cost is only ever paid by somebody who came here to pair something.
///
/// It is never empty, and it cannot be: [`crate::model::Cursor::enter`] will not
/// open an empty column, so a scan that only starts on the way in would be one
/// that could never start at all. The line that stands there instead says what
/// is happening and the one thing a user has to do at the other end — most
/// devices have to be put into pairing mode before anything can hear them.
fn search_page(devices: &[crate::bluetooth::Device]) -> Entry {
    let strangers: Vec<&crate::bluetooth::Device> = devices
        .iter()
        .filter(|device| !device.paired && !device.connected)
        .collect();
    let rows = match strangers.is_empty() {
        true => vec![reading(
            "Looking for devices…",
            "Nothing has answered yet. Most devices have to be put into \
             pairing mode first.",
        )],
        false => strangers.iter().map(|device| device_row(device)).collect(),
    };
    folder(
        SEARCH_PAGE,
        "Look for something new to pair with",
        icons::SEARCH,
        rows,
    )
}

/// One device's row.
///
/// Three shapes, where a network has four, and which one a device gets says
/// what pressing it does. There is no unpressable row here — nothing in
/// Bluetooth is the enterprise network's equivalent, a thing that can be seen
/// and honestly cannot be joined from a console.
///
/// The one that is *connected* is a subcategory carrying the tick — see
/// [`crate::apps::Folder::chosen`] — for the reason the joined network is:
/// connecting to what is already connected is a no-op, and behind that press is
/// the way off it. A device that is merely paired is a subcategory without the
/// tick, because it has two things to do rather than one: come back, or be
/// removed.
///
/// A stranger is a value, and pressing it connects. Whether that means pairing
/// first is BlueZ's answer rather than this page's — see
/// [`crate::bluetooth::Bt::connect`] — which is what keeps a device the user
/// has never met to the single press a list of things to pair with wants.
fn device_row(device: &crate::bluetooth::Device) -> Entry {
    let path = intern(&device.path);
    if device.connected {
        return connected_device(path, device);
    }
    if device.paired {
        return paired_device(path, device);
    }
    value(
        &device.name,
        Some(&device_state(device)),
        false,
        Setting::Bluetooth(BluetoothValue::Connect { device: path }),
    )
}

/// The device this machine is on: what it is, and the two ways off it.
///
/// The facts stand above the pair that act, and the order is the whole of the
/// protection this page gives them — the argument is [`joined_network`]'s,
/// unchanged. A column opens on the row in force and nothing here is in force,
/// so it opens on the first row, and the first row has to be one that does
/// nothing but open something. Somebody stepping into their headphones and
/// pressing Accept twice out of habit lands on `Device information`, not on
/// Forget.
///
/// Between the two that act, Disconnect stands first because it is the one that
/// does less and the one that can be undone from the page it leaves behind.
fn connected_device(path: &'static str, device: &crate::bluetooth::Device) -> Entry {
    chosen_folder(
        &device.name,
        &device_state(device),
        icons::SETTING_BLUETOOTH,
        vec![
            device_information(device),
            action(
                "Disconnect",
                "Come off this device, and keep it paired",
                icons::SETTING_DISCONNECT,
                Setting::Bluetooth(BluetoothValue::Disconnect { device: path }),
            ),
            unpair(path),
        ],
    )
}

/// A device this machine is paired with but not on: connecting to it again,
/// what it is, and forgetting it.
///
/// Connect stands first, so the column opens on it — that is what stepping into
/// something the machine already knows is for, and it keeps the common press to
/// Accept, Accept from the list. The facts stand between the two that act, which
/// is not tidiness: it means the row a wandering thumb comes to rest on is one
/// that changes nothing, and it means Forget is never the row beside the one
/// under the cursor when the column opens.
fn paired_device(path: &'static str, device: &crate::bluetooth::Device) -> Entry {
    folder(
        &device.name,
        &device_state(device),
        icons::SETTING_BLUETOOTH,
        vec![
            action(
                "Connect",
                "Connect to this device again",
                icons::SETTING_CONNECT,
                Setting::Bluetooth(BluetoothValue::Connect { device: path }),
            ),
            device_information(device),
            unpair(path),
        ],
    )
}

/// The row that takes a pairing off the machine.
///
/// One row written once, for the reason [`forget`] is: what it costs is the
/// same whether the device is connected or not, and it is a sentence a user has
/// one chance to read.
///
/// The comment says what goes rather than what happens, again as that one does.
/// "The device will have to be paired with again" is the consequence somebody
/// needs before they press it — not that a bond is deleted, which is true and
/// means nothing to anyone who has not read BlueZ's manual.
///
/// It wears the same waste bin, which is now the mark of *this is taken off the
/// machine* in its third place: an application, a saved network, and a pairing.
fn unpair(device: &'static str) -> Entry {
    action(
        "Forget",
        "Remove this pairing: the device will have to be paired with again",
        icons::UNINSTALL,
        Setting::Bluetooth(BluetoothValue::Forget { device }),
    )
}

/// What one device's row says under its name.
///
/// What it is doing first, because a press that is still being carried out is
/// the only thing on this page the user is actually waiting on — see
/// [`crate::bluetooth::Doing`], which exists because BlueZ has no property that
/// says a pairing is in flight.
///
/// Then what it is to this machine, and then the one further fact that is worth
/// the room: what is left in it while it is connected, and how far off it is
/// while it is not. A paired device the controller cannot hear at all says so,
/// which is the answer to "why will it not connect" for a headset that is flat
/// or in another room.
fn device_state(device: &crate::bluetooth::Device) -> String {
    if let Some(doing) = device.doing {
        return doing.title().to_string();
    }
    let what = match (device.connected, device.paired) {
        (true, _) => "Connected",
        (_, true) => "Paired",
        // Nothing to say about a device the machine has never met but what sort
        // of thing it says it is.
        _ => device.kind.title().unwrap_or("Bluetooth device"),
    };
    let then = match (device.connected, device.paired) {
        (true, _) => device
            .battery
            .map(|left| format!("{left}% battery"))
            .or_else(|| device.kind.title().map(str::to_lowercase)),
        (_, true) => match device.strength {
            None => Some("not in range".to_string()),
            Some(_) => device.kind.title().map(str::to_lowercase),
        },
        _ => nearness(device.strength).map(str::to_string),
    };
    match then {
        Some(then) => format!("{what} — {then}"),
        None => what.to_string(),
    }
}

/// How far off something is, in the words a person would use.
///
/// Three of them, out of an RSSI in dBm. The number itself is on the facts page
/// and not on the row, for the reason a frequency in MHz is not: minus
/// fifty-eight decibel-milliwatts is the shell reading a register out loud, and
/// what the user is actually asking is whether the thing they are holding is
/// the thing at the top of the list.
fn nearness(strength: Option<i16>) -> Option<&'static str> {
    let strength = strength?;
    Some(match strength {
        strength if strength >= -60 => "close by",
        strength if strength >= -75 => "nearby",
        _ => "far away",
    })
}

/// What one device is, as a page to read.
///
/// Behind a row rather than on the page above it, on the terms
/// [`connection_information`] is: these are facts and not settings, and a page
/// whose controls are outnumbered by its footnotes is one a user has to read
/// past to find out there is nothing more to change.
fn device_information(device: &crate::bluetooth::Device) -> Entry {
    let mut values = Vec::new();
    if let Some(kind) = device.kind.title() {
        values.push(("Kind".to_string(), kind.to_string()));
    }
    values.push(("Address".to_string(), device.address.clone()));
    if let Some(battery) = device.battery {
        values.push(("Battery".to_string(), format!("{battery}%")));
    }
    // The number itself, here and only here. A row has to say something a
    // person can act on; a panel of facts is where the reading behind it
    // belongs, exactly as the link speed is.
    if let Some(strength) = device.strength {
        values.push(("Signal".to_string(), format!("{strength} dBm")));
    }
    Entry::Facts(crate::apps::Facts {
        title: "Device information".to_string(),
        comment: device.address.clone(),
        icon: icons::SETTING_INFO.to_string(),
        about: crate::apps::About::Listed(values),
    })
}

/// What this machine is, as far as Bluetooth is concerned: which radio it uses,
/// what it is called to everything else, and what happens to it at startup.
///
/// The page that replaced the column of `hci0`, `hci1`. Everything here is about
/// the machine rather than about anything it is talking to, which is why the
/// controller belongs in it: choosing between two radios is not a thing anybody
/// does twice, and putting it at the top of the tree made the first thing a user
/// saw a question they had no way to answer.
///
/// Two of these five are the shell's own and go in the settings file, and three
/// are BlueZ's. The page does not say which is which and does not need to — see
/// [`apply_with`], where the line is drawn.
fn configuration(
    listing: &crate::bluetooth::Listing,
    controller: &crate::bluetooth::Controller,
) -> Entry {
    let mut rows = Vec::new();
    rows.extend(controller_choice(listing, controller));
    rows.push(bluetooth_name(controller));
    // A fact and not a setting, so a row that cannot be pressed rather than a
    // panel to open: it is one line, and a door in front of one line is a door
    // for its own sake. The information mark is the one [`reading`] carries.
    rows.push(reading("Address", &controller.address));
    rows.push(visibility(controller));
    rows.push(startup_row());
    folder(
        "Configuration",
        "What this machine is called over Bluetooth, and how it comes up",
        icons::SETTING_BLUETOOTH,
        rows,
    )
}

/// Which radio the machine's Bluetooth is — on a machine that has more than
/// one.
///
/// `None` where there is one, which is nearly every machine, and that is the
/// same rule [`wireless`] and [`wired`] collapse under: a list of one is not a
/// choice, and a row offering it would be a row that answers a question nobody
/// asked. What it would have said — the address of the radio in use — is on the
/// row below it either way.
///
/// The controllers are numbered rather than named, and that is not laziness:
/// BlueZ calls every adapter in a machine after the *machine*, so both of them
/// answer to the same name, and the only thing that tells them apart is the
/// address underneath. A number and an address is a row somebody can act on. The
/// kernel's `hci0` is neither.
fn controller_choice(
    listing: &crate::bluetooth::Listing,
    chosen: &crate::bluetooth::Controller,
) -> Option<Entry> {
    if listing.controllers.len() < 2 {
        return None;
    }
    let rows = listing
        .controllers
        .iter()
        .enumerate()
        .map(|(index, controller)| {
            value(
                &format!("Controller {}", index + 1),
                Some(&controller_note(listing, controller)),
                controller.path == chosen.path,
                Setting::Bluetooth(BluetoothValue::Use {
                    address: intern(&controller.address),
                }),
            )
        })
        .collect();
    Some(folder(
        "Controller",
        &chosen.address,
        icons::SETTING_BLUETOOTH,
        rows,
    ))
}

/// What one controller's row says under its number: its address, and what it is
/// doing.
///
/// The address first, because it is the only thing that identifies it. What it
/// is doing is what makes the choice answerable at all — a machine with two
/// radios and one pair of headphones connected can be told which is which by
/// looking.
fn controller_note(
    listing: &crate::bluetooth::Listing,
    controller: &crate::bluetooth::Controller,
) -> String {
    let doing = if !controller.switchable {
        "off at the machine".to_string()
    } else if !controller.powered {
        "off".to_string()
    } else if let Some(connected) = listing
        .devices_of(&controller.path)
        .iter()
        .find(|device| device.connected)
    {
        format!("on {}", connected.name)
    } else {
        "on".to_string()
    };
    format!("{} — {doing}", controller.address)
}

/// What this machine calls itself over Bluetooth.
///
/// The one value in this tree that is typed and is not an address, and the only
/// setting in the whole of Settings that anybody but this machine's owner ever
/// sees: it is what a phone looking for something to pair with puts on its own
/// screen. BlueZ starts it at the machine's host name, which is why it is worth
/// a row — a living room with two consoles in it has two identical entries on
/// every phone that looks.
///
/// It is the controller's `Alias` rather than its `Name`: the name belongs to
/// the machine's configuration and BlueZ will not take one over D-Bus, and the
/// alias is what it publishes when there is one. See
/// [`crate::bluetooth::Bt::rename`].
fn bluetooth_name(controller: &crate::bluetooth::Controller) -> Entry {
    Entry::Typed(crate::apps::Typed {
        title: "Name".to_string(),
        // The panel this opens covers the trail that would say what it is the
        // name *of*. See [`crate::apps::Typed::whose`].
        whose: "Bluetooth".to_string(),
        comment: controller.name.clone(),
        icon: icons::SETTING_TYPED.to_string(),
        value: controller.name.clone(),
        about: Typing::BluetoothName {
            controller: intern(&controller.path),
        },
    })
}

/// Whether anything nearby may find this machine.
///
/// Off is the honest default and BlueZ's own: a machine left discoverable is a
/// machine announcing its name to every radio in the building for as long as it
/// is switched on. It is on this page at all because there is one thing
/// that cannot be done without it — pairing *from the other end*, which is how
/// a phone sends a file to a console and how anything with no screen and no
/// list of its own pairs at all.
///
/// A radio that is off cannot be found, and the row says so rather than
/// offering a switch that BlueZ would refuse.
fn visibility(controller: &crate::bluetooth::Controller) -> Entry {
    if !controller.powered {
        return reading(
            "Visibility",
            "Bluetooth is off, so nothing can find this machine",
        );
    }
    let path = intern(&controller.path);
    let on = controller.discoverable;
    folder(
        "Visibility",
        match on {
            true => "Anything nearby can find this machine",
            false => "Only devices this machine is paired with",
        },
        icons::SETTING_BLUETOOTH,
        vec![
            value(
                "Off",
                Some("Only devices this machine is paired with"),
                !on,
                Setting::Bluetooth(BluetoothValue::Visible {
                    controller: path,
                    on: false,
                }),
            ),
            value(
                "On",
                Some("Anything nearby can find this machine and ask to pair"),
                on,
                Setting::Bluetooth(BluetoothValue::Visible {
                    controller: path,
                    on: true,
                }),
            ),
        ],
    )
}

/// What happens to Bluetooth when a session starts.
///
/// Off above On, as every switch in this tree is, and the third answer under
/// both — because it is not a third state of the radio, it is a refusal to
/// decide, and a row that refuses to decide belongs after the two that do.
///
/// It is [`Startup::Restore`] until somebody says otherwise, which is the one
/// of the three that changes nothing about a machine whose owner has never been
/// to this page.
fn startup_row() -> Entry {
    let now = bluetooth_startup();
    folder(
        "On startup",
        now.title(),
        icons::SETTING_BLUETOOTH,
        vec![
            value(
                "Off",
                Some("Bluetooth is off when the session starts"),
                now == Startup::Off,
                Setting::Bluetooth(BluetoothValue::Startup(Startup::Off)),
            ),
            value(
                "On",
                Some("Bluetooth is on when the session starts"),
                now == Startup::On,
                Setting::Bluetooth(BluetoothValue::Startup(Startup::On)),
            ),
            value(
                "As it was left",
                Some("However the last session left it"),
                now == Startup::Restore,
                Setting::Bluetooth(BluetoothValue::Startup(Startup::Restore)),
            ),
        ],
    )
}

/// Input: how the person in front of the machine talks to it.
///
/// One page under it today and a category anyway, for the reason Games is one:
/// what is here belongs to neither the picture, the sound, the network nor the
/// machine's own behaviour, and the alternative to a category of its own is
/// System — which is the page about how the *machine* behaves, and would then
/// hold a setting about a keyboard between the size applications are drawn at
/// and the version of the kernel.
fn input() -> Entry {
    folder(
        "Input",
        "The keyboard, and the controls in your hands",
        icons::SETTING_INPUT,
        vec![keyboard(), mouse()],
    )
}

/// Everything about typing on this machine: what the keys say, and the board
/// the shell draws when there are no keys.
///
/// **The on-screen keyboard is inside here rather than beside it.** The two
/// pages were siblings under Input for one release and that was wrong: they are
/// not two subjects, they are one subject and a thing about it. The board is a
/// keyboard — it is what a console has instead of the one on the desk — and it
/// already *takes its caps from the layout set on this page*, so a column
/// listing them side by side put the cause and the effect at the same depth and
/// left nothing saying which was which.
///
/// The layout first, then the board, on the same argument one level down: the
/// page that decides comes before the page that follows it, and somebody
/// reading the column downwards meets the question before its consequence.
fn keyboard() -> Entry {
    folder(
        "Keyboard",
        "What the keys say, and the board this shell draws",
        icons::SETTING_KEYS,
        vec![keyboard_layout_page(), on_screen_keyboard()],
    )
}

/// The on-screen keyboard's own page.
///
/// One row today, and a page anyway rather than that row standing directly
/// under Keyboard, on the terms [`retroarch`] makes the same argument on: what
/// is under here belongs to the *board* rather than to keyboards in general,
/// and a setting about how it is summoned arriving beside the layout would
/// leave a column where two rows are about different things and nothing says
/// which.
///
/// **The comment says what the page is, not what one setting on it is set to.**
/// It said "Focused screen" while Default display was the only row under it,
/// which read as the answer to a question this row does not ask — and would
/// have gone on naming one setting out of several the moment a second arrived.
/// A row that opens a page describes the page; the row that carries a value is
/// the one that says what the value is.
fn on_screen_keyboard() -> Entry {
    folder(
        "On-screen keyboard",
        "The board this shell types with",
        icons::SETTING_KEYBOARD,
        vec![keyboard_display_page()],
    )
}

/// The mouse's own page: what the pointer does, and what a wheel does.
///
/// Under Input rather than under Display, which is the one thing about its
/// place worth arguing over. Three of the four rows change what the *screen*
/// shows — the cursor moves, the cursor grows, the page goes by — and none of
/// them is about a screen. What they are about is the thing in somebody's hand
/// and how far it has to travel, which is the same subject the board above them
/// is: a person telling the machine what to do.
///
/// The cursor's two rows first and the wheel's two after, each pair with its
/// speed in front of the other thing about it. A pointer that is too slow to
/// cross the desk is what somebody comes to this page for; a cursor too small
/// to find is the second thing, and both are noticed before anybody thinks
/// about a wheel.
///
/// **Every pointing device, not only the ones that are mice.** A trackball and
/// a touchpad move the same pointer, and the compositor's own config file
/// already has one `[input]` section for all of them — so the page and the file
/// say the same thing about the same devices. The page is called Mouse because
/// that is what somebody is looking for, and a console has one.
fn mouse() -> Entry {
    folder(
        "Mouse",
        "The pointer, and what a wheel does",
        icons::SETTING_MOUSE,
        vec![
            cursor_speed(),
            cursor_size(),
            scrolling_speed(),
            scrolling_direction(),
        ],
    )
}

/// How fast the pointer travels for a given movement of the hand.
///
/// A bar, like the application scale and the night light's temperature, because
/// what is being set is a continuous quantity and the right answer is found by
/// moving until it feels right rather than by reading a name off a list.
///
/// The number on it is **not** libinput's own -100 to 100, which is the one
/// place this page deliberately does not pass the underlying value through. A
/// negative speed is not slower than nothing and 0 is not the slowest — 0 is
/// libinput's *flat default*, in the middle — so a bar labelled with it would
/// have its handle at the middle reading nought and its foot reading minus a
/// hundred. What the row says is where the handle stands on its own track, in
/// per cent, and what goes over the wire is the number libinput means.
fn cursor_speed() -> Entry {
    let speed = pointer().speed;
    let step = |to: i8| {
        (SLOWEST_POINTER..=FASTEST_POINTER)
            .contains(&to)
            .then_some(Setting::Pointer(PointerValue::Speed(to)))
    };
    let span = f32::from(FASTEST_POINTER as i16 - SLOWEST_POINTER as i16);
    folder(
        "Cursor speed",
        &pointer_speed_note(speed),
        icons::SETTING_CURSOR_SPEED,
        vec![Entry::Bar(crate::apps::Bar {
            title: pointer_speed_note(speed),
            comment: Some("How far the pointer travels for a movement of the hand".to_string()),
            fill: f32::from(speed as i16 - SLOWEST_POINTER as i16) / span,
            swatch: None,
            up: step(speed.saturating_add(POINTER_STEP)),
            down: step(speed.saturating_sub(POINTER_STEP)),
            steps: (SLOWEST_POINTER..=FASTEST_POINTER)
                .step_by(POINTER_STEP as usize)
                .map(|speed| Setting::Pointer(PointerValue::Speed(speed)))
                .collect(),
        })],
    )
}

/// Where the pointer's handle stands, as a person reads it.
///
/// Per cent of the track, and the middle says so in words as well: **Default**
/// is the answer somebody wants to be able to get back to, and "50%" is not
/// recognisable as it. See [`cursor_speed`] for why the number is not
/// libinput's.
fn pointer_speed_note(speed: i8) -> String {
    if speed == 0 {
        return "Default".to_string();
    }
    let span = f32::from(FASTEST_POINTER as i16 - SLOWEST_POINTER as i16);
    let along = f32::from(speed as i16 - SLOWEST_POINTER as i16) / span * 100.0;
    format!("{}%", along.round() as i32)
}

/// How large the pointer is drawn.
///
/// A list rather than a bar, unlike the speed above it and the wheel below —
/// see [`CURSOR_SIZES`], where that is argued: a theme draws a handful of sizes
/// and the nearest is used, so the values in between would move the number and
/// not the pointer.
fn cursor_size() -> Entry {
    let size = pointer().size;
    folder(
        "Cursor size",
        cursor_size_note(size),
        icons::SETTING_CURSOR_SIZE,
        CURSOR_SIZES
            .iter()
            .map(|(offered, name)| {
                value(
                    name,
                    Some(&format!("{offered} pixels")),
                    *offered == size,
                    Setting::Pointer(PointerValue::Size(*offered)),
                )
            })
            .collect(),
    )
}

/// What one of those sizes is called, for the row above the list.
fn cursor_size_note(size: u16) -> &'static str {
    CURSOR_SIZES
        .iter()
        .find(|(offered, _)| *offered == size)
        .map(|(_, name)| *name)
        // A size out of a file this shell does not offer. It cannot reach the
        // page — [`nearest_cursor_size`] brings it to one of the four on the
        // way in — but the row says something honest if it ever does.
        .unwrap_or("Its own size")
}

/// How far one movement of a wheel carries the content under it.
///
/// A bar, on the terms [`cursor_speed`] is one, and this one reads as what it
/// is: per cent of the movement the device reported, with 100% the wheel
/// untouched. Unlike the pointer's, the number here *is* the value — a scroll
/// speed is a multiple, and a multiple written as a percentage is the same fact
/// twice rather than a second scale.
fn scrolling_speed() -> Entry {
    let scroll = pointer().scroll;
    let step = |to: u16| {
        (SLOWEST_SCROLL..=FASTEST_SCROLL)
            .contains(&to)
            .then_some(Setting::Pointer(PointerValue::Scroll(to)))
    };
    let span = f32::from(FASTEST_SCROLL - SLOWEST_SCROLL);
    folder(
        "Scrolling speed",
        &format!("{scroll}%"),
        icons::SETTING_SCROLL_SPEED,
        vec![Entry::Bar(crate::apps::Bar {
            title: format!("{scroll}%"),
            comment: Some("How far one turn of a wheel carries the page".to_string()),
            fill: f32::from(scroll.saturating_sub(SLOWEST_SCROLL)) / span,
            swatch: None,
            up: step(scroll.saturating_add(SCROLL_STEP)),
            down: step(scroll.saturating_sub(SCROLL_STEP)),
            steps: (SLOWEST_SCROLL..=FASTEST_SCROLL)
                .step_by(SCROLL_STEP as usize)
                .map(|scroll| Setting::Pointer(PointerValue::Scroll(scroll)))
                .collect(),
        })],
    )
}

/// Which way the content goes.
///
/// Two rows, and they are named after **what moves** rather than after the
/// setting. "Natural" is the word the protocol and every other desktop uses and
/// it is the one word that says nothing: both directions feel natural to
/// whoever is used to them. What tells them apart is which thing follows the
/// hand — the page, or the view of it — so that is what the rows say.
///
/// The traditional direction first, because it is the one a mouse has always
/// had and the one this session comes up in.
fn scrolling_direction() -> Entry {
    let natural = pointer().natural;
    folder(
        "Scrolling direction",
        match natural {
            true => "The page follows your fingers",
            false => "The view follows your fingers",
        },
        icons::SETTING_SCROLL_DIRECTION,
        vec![
            value(
                "Standard",
                Some("Rolling the wheel away moves the view down the page"),
                !natural,
                Setting::Pointer(PointerValue::Natural(false)),
            ),
            value(
                "Natural",
                Some("Rolling the wheel away moves the page itself away"),
                natural,
                Setting::Pointer(PointerValue::Natural(true)),
            ),
        ],
    )
}

/// Which screen the board comes up on: the one being driven, or one named.
///
/// The rows are the screens this session has, taken from what the compositor
/// reports — the same list every page under Display is built from, and there is
/// no other honest one: which screens exist is a fact about what is plugged in,
/// and a fixed list would offer a monitor that is not there.
///
/// **The screen the setting names is offered even when it is not plugged in**,
/// at the end and marked, saying so. It is the same answer
/// [`startup_category_page`] gives a column that is not on the bar, and for the
/// same reason: the setting is still in force — the board comes back to that
/// screen the moment it returns — so a page with nothing ticked on it would be
/// the shell denying a choice it is still keeping. See [`KEYBOARD_DISPLAY`].
///
/// Every row but the first says `Only`, which is doing real work. "DP-1" as a
/// row under "Focused screen" reads as *where the board is now*; "Only DP-1"
/// reads as the rule it actually is, and the difference matters most to the
/// person choosing it with two screens in front of them.
fn keyboard_display_page() -> Entry {
    let chosen = keyboard_display();
    let here: Vec<String> = support().into_iter().map(|(name, _)| name).collect();
    let mut screens = here.clone();
    // The screen the setting names, where it is not one of them. Last, so the
    // rows that lead anywhere come first.
    if let Some(pinned) = chosen.as_deref() {
        if !screens.iter().any(|name| name == pinned) {
            screens.push(pinned.to_string());
        }
    }

    let mut rows = vec![value(
        FOCUSED_SCREEN,
        Some("Wherever the bar is being driven from"),
        chosen.is_none(),
        Setting::KeyboardDisplay(None),
    )];
    rows.extend(screens.iter().map(|name| {
        drawn_value(
            &format!("Only {name}"),
            // A screen that is not plugged in says so, and nothing else says
            // anything: the row is a connector's name, which is what the
            // Display pages call the same screen, and a sentence under each one
            // explaining what DP-1 is would be the cable described back to
            // somebody who plugged it in.
            (!here.contains(name)).then_some("Not plugged in just now"),
            icons::SETTING_DISPLAY,
            chosen.as_deref() == Some(name.as_str()),
            Setting::KeyboardDisplay(Some(intern(name))),
        )
    }));

    folder(
        "Default display",
        &keyboard_display_note(),
        icons::SETTING_DISPLAY,
        rows,
    )
}

/// Which arrangement every keyboard on this machine is set to.
///
/// Three levels — continent, then country, then every arrangement that country
/// has — because as one list it is six hundred rows. The tree is not a
/// classification anybody needs to agree with; it is a way of getting to a
/// hundred rows in three presses, and the field at the head of every column of
/// it is the way for somebody who would rather type the name.
///
/// The arrangements are read off the machine and not written down here, so what
/// is offered is exactly what the compositor beside this shell can compile. See
/// [`crate::layouts`].
fn keyboard_layout_page() -> Entry {
    folder(
        "Keyboard layout",
        &keyboard_layout_note(),
        icons::SETTING_LAYOUT,
        layout_column(|| {
            layouts::registry()
                .continents()
                .into_iter()
                .map(continent_page)
                .collect()
        }),
    )
}

/// One column of the layout tree: the field, and then either what was found or
/// what the column is a list of.
///
/// Every column of the tree is built through here, which is the whole of how
/// one field can be at the head of three different lists and mean the same
/// thing in all of them. `browsing` is only called when there is nothing in the
/// field — building a continent's forty-five countries to then not show them
/// would be work done to be thrown away.
fn layout_column(browsing: impl FnOnce() -> Vec<Entry>) -> Vec<Entry> {
    // A compositor too old to be told is the first thing asked, because it makes
    // every row below it a row that would change nothing. See
    // [`KEYBOARD_LAYOUT_AVAILABLE`].
    if !keyboard_layout_available() {
        return vec![reading(
            "Not offered by this session",
            "The compositor this shell is running on cannot be told a keyboard layout",
        )];
    }
    let registry = layouts::registry();
    let query = layout_query();
    // A machine with no xkb registry has nothing to offer and says so. A column
    // with no rows cannot be stepped into, so the alternative to this row is a
    // row that does not answer when it is pressed.
    if registry.is_empty() {
        return vec![reading(
            "No layouts on this machine",
            "xkeyboard-config is not installed, so there is nothing to choose from",
        )];
    }
    let found = registry.search(&query);
    let mut rows = Vec::new();
    crate::apps::head(
        &mut rows,
        crate::apps::Searched::Layouts,
        &query,
        found.len(),
        registry.len(),
    );
    match query.trim().is_empty() {
        true => rows.extend(browsing()),
        false => rows.extend(found.into_iter().map(found_layout)),
    }
    rows
}

/// One continent, and the countries in it.
fn continent_page(continent: layouts::Continent) -> Entry {
    let countries = layouts::registry().countries_in(continent);
    folder(
        continent.title(),
        &plural(countries.len(), "country", "countries"),
        icons::SETTING_REGION,
        layout_column(|| countries.iter().map(country_page).collect()),
    )
}

/// One country, and every arrangement it claims.
fn country_page(country: &layouts::Country) -> Entry {
    let held = layouts::registry().layouts_in(&country.code);
    folder(
        &country.name,
        &plural(held.len(), "layout", "layouts"),
        icons::SETTING_REGION,
        layout_column(|| {
            held.iter()
                .map(|layout| layout_value(layout, None))
                .collect()
        }),
    )
}

/// One arrangement, as a row somebody standing in a country's column reads.
///
/// The comment is the xkb name, which is the fact that row carries and nowhere
/// else says: it is what goes over the wire, what the settings file is written
/// with, and what somebody who has looked their layout up anywhere else knows
/// it by. A sentence explaining what a keyboard layout is would be the column's
/// own heading read back to somebody standing in it.
fn layout_value(layout: &layouts::Layout, whereabouts: Option<String>) -> Entry {
    let key = layout.key();
    value(
        &layout.name,
        Some(&whereabouts.unwrap_or_else(|| key.clone())),
        keyboard_layout_in_force().as_deref() == Some(key.as_str()),
        Setting::KeyboardLayout(intern(&key)),
    )
}

/// The same row, found by the field rather than walked to.
///
/// It says where it lives instead of what xkb calls it. A found row is out of
/// the tree it belongs to — that is what finding it means — so the fact it is
/// missing is the path somebody would otherwise have walked, and the fact it no
/// longer needs is the name of a column they are not standing in.
fn found_layout(layout: &layouts::Layout) -> Entry {
    let whereabouts = layouts::registry().whereabouts(layout);
    layout_value(layout, Some(whereabouts))
}

/// "1 layout", "9 layouts" — the count a row that opens a column says under
/// its title.
fn plural(count: usize, one: &str, many: &str) -> String {
    match count {
        1 => format!("1 {one}"),
        count => format!("{count} {many}"),
    }
}

/// What the Keyboard layout row says under its title: the arrangement in force,
/// by the name somebody chose it under.
///
/// The registry's own description where this machine has the layout, because
/// that is the row that was pressed. The bare xkb name where it has not — a
/// file naming an arrangement xkeyboard-config does not describe is still the
/// setting, and saying so is better than saying nothing.
fn keyboard_layout_note() -> String {
    if !keyboard_layout_available() {
        return "Not offered by this session".to_string();
    }
    let Some(key) = keyboard_layout_in_force() else {
        return "As this machine is configured".to_string();
    };
    let (layout, variant) = layouts::from_key(&key);
    match layouts::registry().find(&layout, &variant) {
        Some(found) => found.name.clone(),
        None => format!("{key} — not a layout this machine has"),
    }
}

/// What the row above that page says under its title: the screen the board
/// comes up on, in the few words a comment has.
fn keyboard_display_note() -> String {
    match keyboard_display() {
        None => FOCUSED_SCREEN.to_string(),
        Some(name) if screen_is_here(&name) => format!("Only {name}"),
        Some(name) => format!("Only {name} — not plugged in just now"),
    }
}

/// Whether the compositor is reporting a screen of this name at the moment.
fn screen_is_here(display: &str) -> bool {
    support().iter().any(|(name, _)| name == display)
}

/// The first row of that page, and the setting's own default. Named once
/// because it is written in three places — the row, both comments above it —
/// and three copies of it is three chances for them to disagree.
const FOCUSED_SCREEN: &str = "Focused screen";

/// Games: the settings belonging to the games on this machine, as opposed to
/// the machine itself.
///
/// Empty for now, and here anyway. Steam and the games beside it ask the shell
/// for things nothing else does, and the alternative to a page of their own is
/// System — which is the page about how the *machine* behaves, and would then
/// be a page about two unrelated subjects with a user walking past one to reach
/// the other. A console's games are what the console is for; the settings that
/// belong to them are not settings about the hardware.
///
/// It stands in front of System for the reason System is last: a setting about
/// the games is one somebody came here looking for, and the page about the
/// machine itself is the one they arrive at having read past everything they
/// can use.
///
/// The page is not *empty* even so — see [`nothing_to_set_about_games`]. A
/// subcategory the bar refuses to step into is a row that does nothing when
/// pressed, which is the argument [`resolution`] makes about a screen list with
/// no screens in it.
fn games() -> Entry {
    let mut rows = Vec::new();
    // Steam first, and on every machine — including one that has never had
    // Valve's client installed. Unlike the page under it this is not a setting
    // *belonging to* a program: what it decides is whether this shell has a
    // Steam half at all, which is a question about the shell, and somebody who
    // is about to install the client is exactly the person who wants to answer
    // it beforehand.
    rows.push(steam());
    // Only where the package is installed. A page listing a setting belonging
    // to a program this machine has not got would be a page about somebody
    // else's machine — and the row under it opens a picker for a folder
    // nothing would ever read. See [`crate::retroarch::offered`].
    if crate::retroarch::offered() {
        rows.push(retroarch());
    }
    if rows.is_empty() {
        rows.push(nothing_to_set_about_games());
    }
    folder(
        "Games",
        "Steam, and the games on this machine",
        icons::CATEGORY_GAMES,
        rows,
    )
}

/// The page belonging to the Steam integration, which every machine has.
///
/// Under Games rather than under System for the reason the page above it is
/// under Games: what is decided here belongs to the games on this machine, not
/// to the machine. And a page of its own rather than three rows standing
/// directly under Games, for the reason RetroArch has one — what is in here
/// belongs to *Steam*, and the emulator's settings arriving beside them would
/// be two programs' settings in one list with nothing saying which was whose.
///
/// The row above it says what the page is *about* and never what it is set to
/// — see [`steam_note`].
///
/// Two rows or four, and the shape is the argument. The integration's own
/// switch is always here; the two below it are settings *about the client this
/// shell drives*, and a shell that is not driving one has no answer for them —
/// so they are not offered, exactly as a screen list with no screens in it is
/// not. See [`resolution`].
fn steam() -> Entry {
    let mut rows = vec![steam_integration_switch()];
    if steam_integration() {
        rows.push(steam_at_startup_switch());
        rows.push(steam_after_a_game_switch());
        rows.push(steam_other_titles_page());
    }
    // And the one shape that is neither: a session that was told on the command
    // line to leave Steam alone. There is nothing to press — the flag outranks
    // the file, and a switch that said On over a session doing nothing would be
    // the page lying — so the page says why instead of being empty.
    if !*STEAM_IN_THIS_SESSION.lock().unwrap() {
        rows = vec![reading(
            "Steam is not in this session",
            "It was started with --no-steam, so nothing here talks to Steam",
        )];
    }
    folder("Steam", steam_note(), icons::STEAM, rows)
}

/// What the row above that page says under its title: what is in there, and
/// never what it is set to.
///
/// It used to read the three switches back — "On, started with the shell and
/// left running" — and that is a comment doing the wrong job. A row of this
/// tree that opens onto a page is a door, and what a door's label is for is
/// telling somebody whether what they are looking for is behind it. The
/// settings themselves are one press away and each says its own value there;
/// spelling all three out here says nothing to the person who has not been in
/// yet, which is the only person reading it. Every other page in this tree is
/// labelled that way — Games above it says "Steam, and the games on this
/// machine", RetroArch beside it says "Your own games, and how they are
/// played" — and this was the odd one out.
///
/// The one session that gets a different line is the one that cannot press any
/// of it: `--no-steam` is not a setting anybody chose, it is a fact about this
/// session, and it belongs on the door because it is the reason there is
/// nothing behind it.
fn steam_note() -> &'static str {
    match *STEAM_IN_THIS_SESSION.lock().unwrap() {
        true => "How Steam works with the shell",
        false => "Left out of this session",
    }
}

/// Whether this shell drives Valve's client at all — see
/// [`SteamValue::Integration`], which is where the whole of it is argued.
///
/// It wears the mark of the Steam *column*, because the column is the visible
/// half of what it decides: on, the library is a column of the bar; off, there
/// is no column and Steam is a row in Internet like the browser beside it.
fn steam_integration_switch() -> Entry {
    let on = steam_integration();
    folder(
        "Integration",
        match on {
            true => "On — the library is a column of the bar",
            false => "Off — Steam is an application like any other",
        },
        icons::CATEGORY_STEAM,
        vec![
            value(
                "Off",
                Some("Steam is an application like any other, with its own icon"),
                !on,
                Setting::Steam(SteamValue::Integration(false)),
            ),
            value(
                "On",
                Some("Sign in, and play your library from the bar"),
                on,
                Setting::Steam(SteamValue::Integration(true)),
            ),
        ],
    )
}

/// The title of the row that names the tool everything unverified runs under.
///
/// A constant because two things need it and neither may guess at the other's
/// spelling: the row is built here, and the shell asks Steam for the list only
/// while somebody is standing in the column *under* it — see
/// `Shell::standing_in_the_compatibility_tools`. Asking any earlier would wake
/// Valve's client because a cursor walked past a row.
pub const COMPATIBILITY_PAGE: &str = "Compatibility tool";

/// Which Steam Play tool runs the games Valve has not verified — see
/// [`SteamValue::OtherTitles`].
///
/// The list is Valve's client's own and there is no other source for it: what
/// the client offers is what this *account* may use, which on a machine with
/// three Protons installed is eleven names. So the column has the three shapes
/// a fetched list has — coming, arrived, and the reason there is none — and the
/// first of them is what somebody sees for a second or two while the client is
/// asked.
fn steam_other_titles_page() -> Entry {
    let held = COMPATIBILITY.lock().unwrap().clone();
    let rows = match &held {
        None | Some(crate::steam::Compat::Asking) => vec![reading(
            "Asking Steam",
            "Valve's client is being asked which tools this account may use",
        )],
        Some(crate::steam::Compat::Unavailable(why)) => vec![reading("Not now", why)],
        Some(crate::steam::Compat::Said(said)) if said.tools.is_empty() => vec![reading(
            "Steam offers none",
            "This client lists no compatibility tools for this account",
        )],
        Some(crate::steam::Compat::Said(said)) => {
            let mut rows = vec![value(
                "None",
                Some("Games Valve has not verified are not offered a tool"),
                said.forced.is_none(),
                Setting::Steam(SteamValue::OtherTitles(None)),
            )];
            rows.extend(said.tools.iter().map(|tool| {
                value(
                    &tool.display,
                    // The name Steam files it under, which is the fact this row
                    // carries and nowhere else says — the same thing the
                    // keyboard layouts put under theirs.
                    Some(&tool.name),
                    said.forced.as_deref() == Some(tool.name.as_str()),
                    Setting::Steam(SteamValue::OtherTitles(Some(intern(&tool.name)))),
                )
            }));
            rows
        }
    };
    folder(
        COMPATIBILITY_PAGE,
        &steam_other_titles_note(&held),
        icons::SETTING_COMPATIBILITY,
        rows,
    )
}

/// What that row says under its title: what is set, and the one thing about it
/// that would otherwise be a surprise.
///
/// Steam reads this as it comes up and never looks again, which is why its own
/// page offers to restart the client when it is changed. This shell does not
/// restart anything — a client that is up may be fetching somebody's game —
/// so the row says when it takes effect instead of pretending it is immediate.
fn steam_other_titles_note(held: &Option<crate::steam::Compat>) -> String {
    match held {
        Some(crate::steam::Compat::Said(said)) => match said.forced_display() {
            Some(tool) => format!("{tool}, from the next time Steam starts"),
            None => "Games Valve has not verified are not offered a tool".to_string(),
        },
        Some(crate::steam::Compat::Unavailable(why)) => why.clone(),
        _ => "What runs the games Valve has not verified".to_string(),
    }
}

/// Whether the client is started as the session comes up — see
/// [`SteamValue::AtStartup`].
fn steam_at_startup_switch() -> Entry {
    let on = steam_at_startup();
    folder(
        "Start with the shell",
        match on {
            true => "Steam is started in the background as the session comes up",
            false => "Steam is started when the first game is pressed",
        },
        icons::LAUNCH,
        vec![
            value(
                "Off",
                Some("Steam starts when the first game is pressed"),
                !on,
                Setting::Steam(SteamValue::AtStartup(false)),
            ),
            value(
                "On",
                Some("The first game of the day starts as quickly as the second"),
                on,
                Setting::Steam(SteamValue::AtStartup(true)),
            ),
        ],
    )
}

/// Whether the client is left up once a game has ended — see
/// [`SteamValue::AfterAGame`].
fn steam_after_a_game_switch() -> Entry {
    let on = steam_left_after_a_game();
    folder(
        "Leave Steam running",
        match on {
            true => "Steam stays up after a game, so the next one starts sooner",
            false => "Steam is closed with the game, and the memory comes back",
        },
        icons::SHUTDOWN,
        vec![
            value(
                "Off",
                Some("Steam is asked to close when the game, or the press, is over"),
                !on,
                Setting::Steam(SteamValue::AfterAGame(false)),
            ),
            value(
                "On",
                Some("The next game starts in a second or two rather than twenty"),
                on,
                Setting::Steam(SteamValue::AfterAGame(true)),
            ),
        ],
    )
}

/// The page belonging to the RetroArch integration, which exists on a machine
/// that has its package and on no other.
///
/// One row today, and a page anyway rather than that row standing directly
/// under Games: what is under here belongs to *RetroArch* rather than to the
/// games on this machine in general, and a Steam setting arriving beside it
/// would then be two programs' settings in one list with nothing saying which
/// was whose.
fn retroarch() -> Entry {
    let mut rows = Vec::new();
    // The emulators first, one page each, because a person who came to this
    // page came about a game — and what a game looks like is the emulator
    // running it, not the frontend around it. A machine with no core yet has
    // none of these and the page is the shorter for it.
    for core in crate::retroarch::tunables() {
        rows.push(core_page(&core));
    }
    // Then the settings that belong to no core: they are true of every game at
    // once, which is exactly why they come after the pages that are true of
    // one console each.
    rows.push(aspect_ratio());
    rows.push(video_driver());
    rows.push(integer_scale());
    rows.push(vertical_sync());
    rows.push(roms_path());
    rows.push(game_art());
    folder(
        RETROARCH_PAGE,
        "Your own games, and how they are played",
        crate::retroarch::mark(),
        rows,
    )
}

/// What that page is called, for the two things that have to find it.
///
/// It is built here and walked to from the shell — the menu over a console
/// takes the cursor to the page of the emulator that plays it, and that walk
/// looks for the emulator's own name *inside* this one rather than anywhere in
/// the Settings tree. Written once so the two cannot drift apart; a page
/// renamed here would otherwise be a menu row that quietly stopped working.
pub const RETROARCH_PAGE: &str = "RetroArch";

/// One emulator's own settings.
///
/// Nothing on this page is written down in this shell. The core declares what
/// it can be set to and the helper asks it — see [`crate::retroarch::tunables`]
/// — so this page is whatever that emulator's authors put in it, in their
/// order, under their names, and it is right about a version of the core that
/// came out after this shell did.
///
/// Grouped the way the core groups them where it says how, and flat where it
/// does not: an emulator with seventy-five settings sorted into five subjects
/// is a page somebody can walk; the same seventy-five in one column is a list
/// nobody finds anything in.
fn core_page(core: &crate::retroarch::CoreOptions) -> Entry {
    // The console's own BIOS, where this emulator declares one. Above whatever
    // it can be set to, and on this page rather than beside somebody's games,
    // because it belongs to *this* emulator: pcsx2 reads a PlayStation 2 BIOS
    // and the page next to it does not. It is a job rather than a setting, and
    // the one thing on here that decides whether a game starts at all.
    let bios = crate::retroarch::bios_row(&crate::retroarch::firmware_for(&core.core));
    // A core that handed over no table is a row that says so rather than a
    // folder that opens on nothing, because an installed emulator missing from
    // this page altogether reads as an install that failed.
    //
    // Not every emulator answers, even asked all the way. The helper takes a
    // core that declares nothing through `retro_init` and then through
    // `retro_load_game` with no game at all, which is what LRPS2 and dolphin
    // need; one that still says nothing is one that will only speak with a real
    // game in it, and its settings live where that game is running. So the row
    // says where they are and does not promise that playing something will make
    // them turn up here — it will not.
    if core.options.is_empty() {
        // Still a page where there is a BIOS to ask about — which is LRPS2, the
        // one core on this machine that declares nothing and needs a file.
        let Some(bios) = bios else {
            return reading(
                &core.display,
                "Its settings are in RetroArch's own menu, with a game running",
            );
        };
        return folder(
            &core.display,
            "Its BIOS. Everything else is in RetroArch's own menu",
            &crate::retroarch::mark_for_core(&core.core),
            vec![bios],
        );
    }
    let mut rows = Vec::new();
    rows.extend(bios);
    for group in &core.categories {
        let held: Vec<&crate::retroarch::CoreSetting> = core
            .options
            .iter()
            .filter(|option| option.category.as_deref() == Some(group.key.as_str()))
            .collect();
        // A group the core named and then put nothing in is not a row.
        if held.is_empty() {
            continue;
        }
        rows.push(folder(
            &group.title,
            &format!(
                "{} {}",
                held.len(),
                crate::retroarch::plural(held.len(), "setting", "settings")
            ),
            icons::CATEGORY_GAMES,
            held.into_iter()
                .map(|option| core_option_row(&core.display, option))
                .collect(),
        ));
    }
    // Everything the core sorted nowhere — every setting of a core too old to
    // sort them at all, and the odd one left out of a core that does.
    let known: Vec<&str> = core.categories.iter().map(|at| at.key.as_str()).collect();
    let loose: Vec<&crate::retroarch::CoreSetting> = core
        .options
        .iter()
        .filter(|option| match option.category.as_deref() {
            Some(group) => !known.contains(&group),
            None => true,
        })
        .collect();
    if !loose.is_empty() {
        let rest: Vec<Entry> = loose
            .into_iter()
            .map(|option| core_option_row(&core.display, option))
            .collect();
        // Straight onto the page where there is nothing else on it, rather
        // than one folder called "Other" holding the whole emulator.
        if rows.is_empty() {
            rows = rest;
        } else {
            rows.push(folder(
                "Other",
                &format!(
                    "{} {}",
                    rest.len(),
                    crate::retroarch::plural(rest.len(), "setting", "settings")
                ),
                icons::CATEGORY_GAMES,
                rest,
            ));
        }
    }

    let count = core.options.len();
    folder(
        &core.display,
        &format!(
            "{count} {}",
            crate::retroarch::plural(count, "setting", "settings")
        ),
        // The console's own mark rather than RetroArch's. Four emulators under
        // one page all wearing the frontend's drawing is four rows told apart
        // only by reading them, and what somebody is looking for on this page
        // is the machine — see [`crate::retroarch::mark_for_core`].
        &crate::retroarch::mark_for_core(&core.core),
        rows,
    )
}

/// One of a core's settings, and the values it will take.
///
/// The line under the name is what it is set to *now*, which is the one thing
/// a row like this has to say: a page of thirty names with no answers beside
/// them is a page somebody has to walk into thirty times to read.
fn core_option_row(core: &str, option: &crate::retroarch::CoreSetting) -> Entry {
    // What the emulator would use: the value somebody chose, or the core's own
    // default where nobody has. RetroArch reads an absent line exactly that
    // way, which is why an absent line is not worth writing.
    let now = crate::retroarch::core_option(core, &option.key)
        .or_else(|| option.default.clone())
        .unwrap_or_default();
    let said = option
        .values
        .iter()
        .find(|value| value.value == now)
        .map(|value| value.label.clone())
        .unwrap_or_else(|| now.clone());

    let core = intern(core);
    let key = intern(&option.key);
    let values = option
        .values
        .iter()
        .map(|value| {
            self::value(
                &value.label,
                None,
                value.value == now,
                Setting::CoreOption {
                    core,
                    key,
                    value: intern(&value.value),
                },
            )
        })
        .collect();
    folder(&option.title, &said, icons::SWATCH, values)
}

/// A setting of RetroArch's own: one key, and the values it takes.
///
/// `values` are `(what is written, what it is called)`, in the order they are
/// offered. `fallback` is what RetroArch uses when the line is not in its
/// configuration at all, so that the page says which value is in force on a
/// machine where nobody has ever set it.
fn emulator_choice(
    title: &str,
    icon: &str,
    key: &'static str,
    fallback: &str,
    values: &[(&str, &str)],
) -> Entry {
    let now = crate::retroarch::setting(key).unwrap_or_else(|| fallback.to_string());
    let said = values
        .iter()
        .find(|(value, _)| *value == now)
        .map(|(_, label)| (*label).to_string())
        .unwrap_or_else(|| now.clone());
    let rows = values
        .iter()
        .map(|(written, label)| {
            self::value(
                label,
                None,
                *written == now,
                Setting::Emulator {
                    key,
                    value: intern(written),
                },
            )
        })
        .collect();
    folder(title, &said, icon, rows)
}

/// The shape every game is drawn at.
///
/// A short list out of RetroArch's own long one. Its values are *positions* in
/// a table the emulator carries, so only the ones worth offering are offered:
/// what somebody in front of a television wants is the console's own shape, a
/// television's shape, or pixels that are square — and the twenty ratios
/// between those are a menu nobody reads.
///
/// `22` is Core provided, and it is RetroArch's own default — which is how it
/// is known to be 22 rather than assumed: it is the number in the
/// configuration of a machine where nobody has ever touched this.
fn aspect_ratio() -> Entry {
    emulator_choice(
        "Aspect ratio",
        icons::SETTING_RESOLUTION,
        "aspect_ratio_index",
        "22",
        &[
            ("22", "As the console had it"),
            ("21", "Square pixels"),
            ("0", "4:3"),
            ("1", "16:9"),
            ("24", "Fill the screen"),
        ],
    )
}

/// What RetroArch draws with.
///
/// The three this machine's build has, which `retroarch --features` is what
/// says. A driver that is not there is not a row: RetroArch would fall back to
/// one that is and the page would be saying something untrue.
fn video_driver() -> Entry {
    emulator_choice(
        "Video driver",
        icons::SETTING_DISPLAY,
        "video_driver",
        "gl",
        &[
            ("gl", "OpenGL"),
            ("glcore", "OpenGL (core profile)"),
            ("vulkan", "Vulkan"),
            ("sdl2", "Software"),
        ],
    )
}

/// Whether a game is drawn at a whole multiple of its own size.
fn integer_scale() -> Entry {
    emulator_choice(
        "Whole-number scaling",
        icons::SETTING_SCALE,
        "video_scale_integer",
        "false",
        &[("false", "Off"), ("true", "On")],
    )
}

/// Whether a game waits for the screen.
fn vertical_sync() -> Entry {
    emulator_choice(
        "Wait for the screen",
        icons::SETTING_REFRESH,
        "video_vsync",
        "true",
        &[("true", "On"), ("false", "Off")],
    )
}

/// Where somebody keeps their games.
///
/// The row is built by the integration itself and used in two places — here,
/// and at the head of the RetroArch column — because it is one setting: two
/// rows that opened the same picker and said different things about it would
/// be two settings on screen. See [`crate::retroarch::RetroArch::folder_row`].
///
/// It opens a column of folders rather than a panel, which is what the wallpaper
/// picker two pages away does and for the same reason: a place on the disk is
/// answered by walking to it, and this bar walks with a column.
fn roms_path() -> Entry {
    let Entry::Folder(mut row) = crate::retroarch::folder_row() else {
        unreachable!("it is a folder");
    };
    // Under Settings it is a row of the page like any other rather than
    // something standing over a list: there is no list here for it to stand
    // over, and a page whose only row could not be landed on would be a page
    // that cannot be used.
    row.over_the_list = false;
    row.title = "ROMs path".to_string();
    Entry::Folder(row)
}

/// The row that fetches every game's cover and screenshot again.
///
/// The shell asks for these itself, once per folder per session, for whatever
/// has not got them — so this row is not how somebody gets their artwork. It is
/// how they get it *again*, and there are two reasons to want that: libretro's
/// collection grows, so a game that had no cover last year may have one now;
/// and a game the shell could not put a name to has very often been renamed
/// since, which is what the Rename row over it is for.
///
/// It sits at the bottom, under the folder it is about, because it is the one
/// row on this page that is a press rather than an answer — see [`action`].
fn game_art() -> Entry {
    action(
        "Get the artwork again",
        "Look for a cover and a picture for every game, from libretro",
        icons::SETTING_WALLPAPER,
        Setting::EmulatorArt,
    )
}

/// The row that stands in for the Games page when there is nothing on it.
///
/// Unreachable on an ordinary machine and kept all the same: the Steam page
/// above is built on every machine, so the only way here is a build with that
/// row taken out. It says what the page is *for* rather than only that it is
/// empty — a user who opens this page has a question, and "nothing here" alone
/// would leave them wondering whether they had come to the wrong place.
fn nothing_to_set_about_games() -> Entry {
    reading(
        "Nothing to set here yet",
        "The settings for Steam and the other games on this machine will be on \
         this page",
    )
}

/// System: how the machine behaves, as opposed to what its picture and its
/// speakers are doing.
///
/// The scale is the row the page exists for rather than the page being a place
/// to put things: how large applications draw themselves is neither a property
/// of a display — the two screens on a desk want the same answer, because it is
/// the person in front of them who has to read it — nor anything the shell does
/// to itself, which is what Appearance holds.
///
/// System information is under it and not above it, and that order is the one
/// thing about this page worth arguing over. The page is a page of settings, so
/// the setting comes first; the row that changes nothing is the one a user
/// arrives at last, having read past the one they can use. It is here at all
/// because there is nowhere else it could be — a fact about the machine is not
/// about a display and is not about how the shell looks — and because a console
/// that cannot say what it is is a console nobody can be helped over a
/// telephone with.
fn system(bar: &[crate::apps::Column]) -> Entry {
    folder(
        "System",
        "How the machine behaves",
        icons::SETTING_SYSTEM,
        vec![
            startup_category_page(bar),
            application_scale(),
            picture_in_picture_page(),
            button_hints_switch(),
            system_information(),
        ],
    )
}

/// Startup category: which column of the start screen a session opens on.
///
/// First on this page, in front of the two settings about applications. It is
/// the one setting here about the *shell's own front door* — what a person sees
/// before they have pressed anything — and everything else on the page is about
/// what happens after that.
///
/// The rows are the columns this machine has, in the order they stand on the
/// bar, each wearing its own mark. There is no other honest list: which columns
/// exist is a fact about what is installed, what is signed in and what is
/// plugged in, and a fixed list would offer Waydroid on a machine that has none
/// and hide a Steam library from somebody who has one.
///
/// The row itself wears the mark of the column that is chosen, and its comment
/// names it — the third row in this tree whose glyph moves, after the battery's
/// and the floating window's corner, and it moves for their reason: what is
/// being chosen *is* a column, so the row somebody opens is headed by the answer
/// they are deciding about rather than by a picture of the whole bar.
///
/// A column named by the setting but not on the bar this session is offered all
/// the same, at the end and marked, so that a setting somebody chose is a
/// setting they can still see. It is what happens when an account is signed out
/// or a package removed, and the alternative is a page with nothing chosen on
/// it and no explanation.
fn startup_category_page(bar: &[crate::apps::Column]) -> Entry {
    let chosen = startup_category();
    let mut columns: Vec<crate::apps::Column> = bar.to_vec();
    if !columns.iter().any(|column| column.id == chosen) {
        if let Some(absent) = crate::apps::known_column(&chosen) {
            columns.push(absent);
        }
    }

    let named = columns
        .iter()
        .find(|column| column.id == chosen)
        .copied()
        // A hand-edited file naming a column no version of this shell has ever
        // had. Nothing is marked and the page says so, which is the honest
        // answer: the file is one the user is entitled to open.
        .unwrap_or(crate::apps::Column {
            id: "",
            title: "Not on this machine",
            icon: icons::CATEGORY_OTHER,
        });

    folder(
        "Startup category",
        named.title,
        named.icon,
        columns
            .iter()
            .map(|column| {
                drawn_value(
                    column.title,
                    startup_note(column, bar),
                    column.icon,
                    column.id == chosen,
                    Setting::StartupCategory(column.id),
                )
            })
            .collect(),
    )
}

/// What one row of that page says under its title.
///
/// Two things, and only one of them per row. A column that is not on the bar
/// says so, because a row offering to open on something that is not there owes
/// the user that much. Everything else says nothing: the row is a column's name
/// and its own mark, which is exactly how that column is drawn on the bar, and
/// a sentence under each one explaining what Multimedia is would be the bar
/// described back to somebody who is looking at it.
fn startup_note(column: &crate::apps::Column, bar: &[crate::apps::Column]) -> Option<&'static str> {
    (!bar.iter().any(|had| had.id == column.id)).then_some("Not on this machine just now")
}

/// The legends, on or off.
///
/// Off and On in that order and marked the way every other switch in this tree
/// is — see [`battery_percent_switch`], which is the same question asked about
/// the corner.
///
/// After the two settings about applications and before the machine's own
/// facts, which is the order this page keeps: it is a setting, so it comes
/// above the row that changes nothing, and it is about the shell rather than
/// about what the shell runs, so it comes below the two that are not.
///
/// The rows say what a legend *is* rather than what the switch does, because
/// somebody who has turned it off can no longer see the thing being described.
/// "Pictures of the buttons, and the word for what each one does" is the answer
/// to "what did I just turn off", and a row reading "Show hints" would not be.
///
/// **It says every screen rather than naming them**, and that is deliberate:
/// the start screen, the menu, the friends list, the corner chip that summons
/// the keyboard and the foot of an application's file question all read this
/// one value, and so does anything built on the toolkit. A row that listed them
/// would be a row to be corrected every time the shell grew another screen.
fn button_hints_switch() -> Entry {
    let on = button_hints();
    folder(
        "Button hints",
        "What the buttons do, written where they are used",
        icons::PAD_SOUTH,
        vec![
            value(
                "Off",
                Some("No screen says which button does what"),
                !on,
                Setting::ButtonHints(false),
            ),
            value(
                "On",
                Some("A picture of each button, and the word for what it does"),
                on,
                Setting::ButtonHints(true),
            ),
        ],
    )
}

/// Picture-in-Picture: what happens to the small window a browser puts a video
/// into when the user asks for one.
///
/// Under System rather than under Appearance, and it is worth saying why: what
/// this changes is not how the shell looks but what the *compositor* does with
/// a window — where it puts it, how large it configures it, whether it is given
/// the keyboard and what it is drawn in front of. That is the same kind of
/// setting as the application scale it stands beside, and the same half of the
/// session carries both out.
///
/// After the scale and before the machine's own facts, which is the order this
/// page keeps everywhere: the setting somebody came here to change first, and
/// the row that changes nothing last.
///
/// Three rows, and the page's comment says what all three are set to at once.
/// A user who opens System is deciding whether to come in here at all, and
/// "On, medium, top right" answers that without a press.
fn picture_in_picture_page() -> Entry {
    let pip = picture_in_picture();
    folder(
        "Picture-in-Picture",
        &floating_summary(pip),
        icons::SETTING_PIP,
        vec![
            picture_in_picture_switch(pip),
            picture_in_picture_size(pip),
            picture_in_picture_place(pip),
        ],
    )
}

/// What the floating window is set to, in the few words a row's comment has.
///
/// Off is the whole answer when it is off: a size and a corner for a window
/// that never floats would be describing something the user cannot see.
fn floating_summary(pip: Pip) -> String {
    if !pip.floating {
        return "Off".to_string();
    }
    format!(
        "On, {}, {}",
        pip.size.title().to_lowercase(),
        pip.place.title().to_lowercase()
    )
}

/// The switch itself, on or off.
///
/// Off and On in that order and marked the way every other switch in this tree
/// is. What Off means is spelled out on the row rather than left to be
/// discovered: the window does not disappear, it stops being *special* — it
/// fills the screen like every other window, and the video is then something
/// the user has to switch away from rather than something beside what they are
/// doing.
fn picture_in_picture_switch(pip: Pip) -> Entry {
    folder(
        "Picture-in-Picture",
        "Float a browser's video window over everything else",
        icons::SETTING_PIP,
        vec![
            value(
                "Off",
                Some("Such a window fills the screen, like every other one"),
                !pip.floating,
                Setting::PictureInPicture(PipValue::Floating(false)),
            ),
            value(
                "On",
                Some("It floats in a corner, over applications and over the guide"),
                pip.floating,
                Setting::PictureInPicture(PipValue::Floating(true)),
            ),
        ],
    )
}

/// How large it is drawn: three shares of the display's width.
///
/// A list and not a bar, unlike the application scale above it. The scale is a
/// quantity with no steps in it — every five per cent is a sensible answer — and
/// this is three answers to *how much of the screen am I willing to give up*,
/// which is a question with about three answers in it. Each row says what its
/// share is, because a size named Medium tells nobody anything.
///
/// A width, and only a width. How tall the window is at that width is the
/// window's own business — the compositor asks it what shape it wants to be —
/// so there is nothing here for a user to set about it, and a page that asked
/// would be asking them the aspect ratio of their own video.
fn picture_in_picture_size(pip: Pip) -> Entry {
    folder(
        "Size",
        pip.size.title(),
        icons::SETTING_PIP_SIZE,
        pip::Size::ALL
            .iter()
            .map(|size| {
                value(
                    size.title(),
                    Some(match size {
                        pip::Size::Small => "A sixth of the screen across",
                        pip::Size::Medium => "A quarter of it",
                        pip::Size::Large => "A third of it",
                    }),
                    pip.size == *size,
                    Setting::PictureInPicture(PipValue::Size(*size)),
                )
            })
            .collect(),
    )
}

/// Which corner it sits in: the four of them, drawn.
///
/// The second set of values in this tree with drawings of their own, after the
/// four orientations — and they earn the exception the same way. What is being
/// chosen *is* a place, so the row that matches the corner somebody wants can
/// be picked out without reading it, and four rows reading Top left, Top right,
/// Bottom left, Bottom right are four rows nobody can tell apart at a glance.
///
/// Reading order, which is also the order the corners are numbered in on the
/// wire: the two at the top, then the two at the bottom.
///
/// The row itself wears the corner that is chosen, so the list somebody opens
/// is headed by the answer they are deciding about rather than by a picture of
/// four corners at once. It is the second row in this tree whose glyph moves —
/// the battery's is the first — and it moves for the same reason.
fn picture_in_picture_place(pip: Pip) -> Entry {
    folder(
        "Placement",
        pip.place.title(),
        corner_glyph(pip.place),
        pip::Place::ALL
            .iter()
            .map(|place| {
                drawn_value(
                    place.title(),
                    None,
                    corner_glyph(*place),
                    pip.place == *place,
                    Setting::PictureInPicture(PipValue::Place(*place)),
                )
            })
            .collect(),
    )
}

/// The drawing for one corner: a screen with the small window standing in that
/// corner of it.
fn corner_glyph(place: pip::Place) -> &'static str {
    match place {
        pip::Place::TopLeft => icons::SETTING_PIP_TOP_LEFT,
        pip::Place::TopRight => icons::SETTING_PIP_TOP_RIGHT,
        pip::Place::BottomLeft => icons::SETTING_PIP_BOTTOM_LEFT,
        pip::Place::BottomRight => icons::SETTING_PIP_BOTTOM_RIGHT,
    }
}

/// System information: what this machine is, as a panel to read.
///
/// A door rather than a page of the bar. Everything behind it is a named value
/// with an answer beside it — nine of them, most too long to be a row's comment
/// — and a column offering them as rows would be a list the user has to walk
/// down to read, one fact at a time, in a shell where walking down a list is
/// how a value is *chosen*. The panel puts the whole of it in front of them at
/// once and takes one button to leave, which is what the console this bar comes
/// from did with the same page.
///
/// The information mark rather than the chip: the chip is the System page's
/// own glyph and is on the row this column was opened from, one step to the
/// left and still on screen. This row is the shell's other read-only mark, the
/// one [`reading`] carries — see [`icons::SETTING_INFO`].
///
/// Nothing is read here. The row is built every time the tree is, which is
/// often and for reasons that have nothing to do with this page; the facts are
/// read on the press instead, so what the panel shows is the machine as it is
/// at the moment it is asked. See [`crate::machine`].
fn system_information() -> Entry {
    Entry::Facts(crate::apps::Facts {
        title: "System information".to_string(),
        comment: "What this machine is".to_string(),
        icon: icons::SETTING_INFO.to_string(),
        about: crate::apps::About::Machine,
    })
}

/// Application scaling: how large every application draws its own interface.
///
/// A bar, for the reason the colour temperature is one: what is being set is a
/// *scale* and not a set of alternatives. Every five per cent between one to one
/// and three times is a sensible answer, which as rows is forty-one of them —
/// a column nobody can scan, standing for a quantity that has no steps in it to
/// begin with. On a bar the whole range is under the cursor at once, Up and Down
/// mean what they mean in every other column, and Left still leaves.
///
/// No swatch. The night light's bar is drawn in the colour of the light it
/// stands for, because that bar is a picture of what the screen is about to look
/// like and nothing else can be; a size has no colour, and tinting this one
/// would be saying something about the setting that is not true.
///
/// The floor is one to one and the bar starts there — see [`NATURAL_SCALE`]. It
/// is not a range with a neutral point in the middle: below it an application
/// would be asked to draw its interface *smaller* than it chose, which is a
/// thing to want at a desk and not on the screen this shell is for.
fn application_scale() -> Entry {
    let percent = app_scale();
    let step = |to: u16| {
        (NATURAL_SCALE..=LARGEST_SCALE)
            .contains(&to)
            .then_some(Setting::AppScale(to))
    };
    let span = (LARGEST_SCALE - NATURAL_SCALE) as f32;
    folder(
        "Application scaling",
        &format!("{percent}% — {}", scale_note(percent).to_lowercase()),
        icons::SETTING_SCALE,
        vec![Entry::Bar(crate::apps::Bar {
            title: format!("{percent}%"),
            comment: Some(scale_note(percent).to_string()),
            fill: (percent - NATURAL_SCALE) as f32 / span,
            swatch: None,
            up: step(percent.saturating_add(SCALE_STEP)),
            down: step(percent.saturating_sub(SCALE_STEP)),
            // The same steps a direction walks, all of them, so a click along
            // the groove reaches the one it landed on directly.
            steps: (NATURAL_SCALE..=LARGEST_SCALE)
                .step_by(SCALE_STEP as usize)
                .map(Setting::AppScale)
                .collect(),
        })],
    )
}

/// One to one: every application at the size it chose, and the foot of the bar.
///
/// The whole range is above it. See [`application_scale`], and the compositor's
/// own `scale` module, which clamps to the same floor — the two agree, and the
/// one that matters is the compositor's, because it is the one applications are
/// configured by.
pub const NATURAL_SCALE: u16 = 100;

/// The head of the bar.
///
/// Three times over is already an interface with a third of the room it was
/// designed for, which is where an application's own dialogs start arriving
/// larger than the screen that has to hold them. The compositor stops here too.
pub const LARGEST_SCALE: u16 = 300;

/// How far one press moves the bar.
///
/// Five per cent, which is the smallest step that is a visible change to a line
/// of text — and it puts the whole range forty presses from end to end, which a
/// held direction crosses in a moment. It also divides the range exactly, so the
/// head of the bar is a step the user can actually land on.
const SCALE_STEP: u16 = 5;

/// What a scale means, in the words a number cannot carry.
///
/// Bands rather than a phrase per step, because five per cent is not a
/// difference anybody has a separate name for — and strictly larger down the
/// list, so a bar walked in one direction never reads as turning back.
///
/// The foot of the track is a band of its own. 100% is the one size at which
/// this setting does nothing whatever, and one step above it is an application
/// that has been changed, however slightly: a row that said "its own size"
/// there would be saying the setting had not taken.
fn scale_note(percent: u16) -> &'static str {
    match percent {
        0..=100 => "Every application at its own size",
        101..=115 => "A little larger than the application chose",
        116..=135 => "Comfortable from an armchair",
        136..=165 => "Half again as large",
        166..=199 => "Large: made to be read across a room",
        200..=249 => "Twice the size, and most windows still fit",
        250..=299 => "Very large; some windows will run out of room",
        _ => "As far as this goes, and further than most windows go",
    }
}

// --- Settings > Users -------------------------------------------------------

/// Who this machine is for: every account on it, and the row that makes
/// another.
///
/// The page is a list of *people*, and it is the only page in this tree whose
/// rows wear photographs rather than marks. That is not decoration: an account
/// is a person, and the thing that says which person is their own picture. A
/// row with no picture falls back to the single figure — deliberately not the
/// two figures this row itself wears, or every row on the page would be wearing
/// the heading above it. See [`icons::SETTING_USERS`].
///
/// Add user is at the foot rather than the head, unlike the field over a shelf
/// or the row that answers a folder picker. Those stand over their columns
/// because they are *about* the column; this one is not — it is one more thing
/// the page can do, and the people are what somebody came here to find. A page
/// that opened on it would begin every visit one step below where it meant to.
fn users() -> Entry {
    let listing = crate::users::listing();
    if !listing.daemon {
        // Never an empty column: the bar refuses to step into one, so a machine
        // with no account service would have a row that silently did nothing.
        // The same bargain the Network page states, in the same words.
        return folder(
            "Users",
            "Who this machine is for",
            icons::SETTING_USERS,
            vec![reading(
                "No account service is running",
                "Without accounts-daemon nothing here can read or change who has \
                 an account on this machine; they are managed outside this session",
            )],
        );
    }
    let mut rows = Vec::new();
    // What went wrong with the last press, on a row of its own at the head of
    // the page. At the head because it is about the page rather than about one
    // account, and because a refusal that appeared below the fold would be a
    // press that looked as though it had done nothing.
    if let Some(trouble) = listing.trouble.as_ref() {
        rows.push(reading(&trouble.what, &trouble.why));
    }
    rows.extend(
        listing
            .people
            .iter()
            .map(|person| person_row(person, &listing)),
    );
    rows.push(add_user_row(&listing));
    folder("Users", &users_note(&listing), icons::SETTING_USERS, rows)
}

/// What the Users row says before it is stepped into.
fn users_note(listing: &crate::users::Listing) -> String {
    match listing.people.len() {
        0 => "No accounts on this machine".to_string(),
        1 => "1 account on this machine".to_string(),
        many => format!("{many} accounts on this machine"),
    }
}

/// One account's row: their picture, their name, and the form behind it.
fn person_row(person: &crate::users::Person, listing: &crate::users::Listing) -> Entry {
    let whose = crate::users::Whose::Existing(person.uid);
    let Entry::Folder(mut inner) = folder(
        person.title(),
        &person.note(),
        icons::SETTING_PERSON,
        form_rows(whose, Some(person), listing),
    ) else {
        unreachable!("folder builds a folder");
    };
    // The picture the account names, where it names one that is really there.
    // A row that asked the thumbnailer for a file somebody had deleted would
    // ask once a frame for the rest of the session; the check is in
    // [`crate::users::Person::picture`], which is where the file is read.
    inner.portrait = person.picture.clone();
    inner.person = Some(whose);
    Entry::Folder(inner)
}

/// The row at the foot of the page that makes an account.
fn add_user_row(listing: &crate::users::Listing) -> Entry {
    let whose = crate::users::Whose::New;
    let Entry::Folder(mut inner) = folder(
        "Add user",
        "Make another account on this machine",
        icons::SETTING_ADD_USER,
        form_rows(whose, None, listing),
    ) else {
        unreachable!("folder builds a folder");
    };
    // The picture chosen on the form, once one has been. It is the only row on
    // this page that is not an account and still wears a face — which is the
    // point: the row is what the account is going to be.
    inner.portrait = crate::users::form_for(whose).and_then(|form| form.picture);
    inner.person = Some(whose);
    Entry::Folder(inner)
}

/// The form: everything an account is, and the row that hands it over.
///
/// Drawn from the draft where one is open for this account, and from the
/// account itself where none is. Both, rather than one or the other, because
/// this column is built long before anybody steps into it — the whole Settings
/// tree is rebuilt several times a minute — and it has to say something true on
/// every one of those rebuilds. Before the press it says what the account *is*;
/// after it, what the form has been made to say. See [`crate::users`], where the
/// draft lives, and `Shell::sync_user_form`, which opens and closes it.
///
/// The order is what somebody fills in, in the order they think of it: who this
/// is, what they log in as, what they are allowed to do, what they type to get
/// in, and last the picture — which is the one thing on the form that is not
/// needed for the account to work.
fn form_rows(
    whose: crate::users::Whose,
    person: Option<&crate::users::Person>,
    listing: &crate::users::Listing,
) -> Vec<Entry> {
    use crate::users::Field;
    let form = crate::users::form_for(whose);
    let real = match form.as_ref() {
        Some(form) => form.real.clone(),
        None => person.map(|person| person.real.clone()).unwrap_or_default(),
    };
    let name = match form.as_ref() {
        Some(form) => form.name.clone(),
        None => person.map(|person| person.name.clone()).unwrap_or_default(),
    };
    let admin = match form.as_ref() {
        Some(form) => form.admin,
        None => person.is_some_and(|person| person.admin),
    };
    let picture = match form.as_ref() {
        Some(form) => form.picture.clone(),
        None => person.and_then(|person| person.picture.clone()),
    };
    let whom = whose_form(person);

    let mut rows = vec![
        typed_user(whose, Field::Name, &real, &whom),
        username_row(whose, person, &name, &whom),
    ];
    rows.push(account_type_row(whose, person, admin, listing));
    rows.push(password_row(
        whose,
        Field::Password,
        form.as_ref().map_or(0, |form| form.typed),
        person.is_some(),
        &whom,
    ));
    // The second field only where the password is being set on an account
    // nobody can get into yet — see [`crate::users::asks_twice`], which is
    // where that is argued and which the check at the foot of the form reads
    // too, so the two cannot disagree.
    if crate::users::asks_twice(whose) {
        rows.push(password_row(
            whose,
            Field::Confirm,
            form.as_ref().map_or(0, |form| form.confirmed),
            person.is_some(),
            &whom,
        ));
    }
    rows.push(avatar_row(picture.as_deref()));
    rows.push(accept_row(whose, person, listing));
    if let Some(person) = person {
        rows.push(removal_row(person, listing));
    }
    rows
}

/// What the panel raised over a form's field is subtitled — see
/// [`crate::apps::Typed::whose`].
///
/// The account it is about, because a field raised over the bar covers the trail
/// that would otherwise say so, and "Username" on its own is the same panel on
/// both forms.
fn whose_form(person: Option<&crate::users::Person>) -> String {
    match person {
        Some(person) => person.title().to_string(),
        None => "New account".to_string(),
    }
}

/// One of the two fields on a form that is typed and is not a secret.
/// What they log in as — a field, unless it is one nothing could change.
///
/// An account somebody is signed in to cannot be renamed: `usermod -l` refuses
/// for anything with a process running, and no flag overrides it. So the row
/// does not offer it. Refusing on the panel afterwards was the first version of
/// this and it was the wrong half of the bargain — a row that opens a keyboard,
/// takes a name and then says no is a row that wasted somebody's time to tell
/// them what it knew before they pressed it.
///
/// It still *says* the name, which is why this is a reading and not a row that
/// disappears. What somebody logs in as is worth knowing on a page about them,
/// and a form that silently lost a field between one account and the next would
/// read as a shell that had failed to draw it. The same shape the sole
/// administrator's Account type takes, and for the same reason.
fn username_row(
    whose: crate::users::Whose,
    person: Option<&crate::users::Person>,
    name: &str,
    whom: &str,
) -> Entry {
    use crate::users::Field;
    if let Some(person) = person.filter(|person| person.here) {
        let why = match person.you {
            true => "you are signed in as it",
            false => "they are signed in",
        };
        return reading_marked(
            Field::Username.title(),
            &format!(
                "{} — {why}, and Linux will not rename an account in use",
                person.name
            ),
            icons::SETTING_USERNAME,
        );
    }
    typed_user(whose, Field::Username, name, whom)
}

fn typed_user(
    whose: crate::users::Whose,
    field: crate::users::Field,
    value: &str,
    whom: &str,
) -> Entry {
    Entry::Typed(crate::apps::Typed {
        title: field.title().to_string(),
        whose: whom.to_string(),
        comment: match value.trim().is_empty() {
            // A value nobody has set says so in words rather than leaving the
            // line blank, which reads as a row that failed to load.
            true => "Not set".to_string(),
            false => value.trim().to_string(),
        },
        icon: match field {
            crate::users::Field::Username => icons::SETTING_USERNAME.to_string(),
            _ => icons::SETTING_NAME.to_string(),
        },
        value: value.to_string(),
        about: Typing::User { whose, field },
    })
}

/// One of the two that is.
///
/// It carries no value, and that is the difference that matters: every other
/// typed row in this tree opens holding what the setting already is, so that
/// changing the last number of an address does not mean typing the other three
/// again. A password cannot, because the shell has not got it — what is in
/// `/etc/shadow` is a hash, and nothing anywhere can turn one back into what was
/// typed. The line under the row says how many characters are waiting instead.
fn password_row(
    whose: crate::users::Whose,
    field: crate::users::Field,
    typed: usize,
    exists: bool,
    whom: &str,
) -> Entry {
    let comment = match (typed, exists) {
        (0, true) => "Unchanged".to_string(),
        (0, false) => "Not set".to_string(),
        (1, _) => "1 character".to_string(),
        (typed, _) => format!("{typed} characters"),
    };
    Entry::Typed(crate::apps::Typed {
        title: field.title().to_string(),
        whose: whom.to_string(),
        comment,
        // The key, not the padlock. A padlock is the thing that is *locked* —
        // which is what the panel polkit raises wears, and what the row at the
        // foot of this form now leads to — and a key is what somebody types to
        // get past one. Both password rows share it: they are one question
        // asked twice.
        icon: icons::SETTING_PASSWORD.to_string(),
        // Never anything: see the note above. `Written::for_field` in `main`
        // ignores it for a secret field and opens an empty one.
        value: String::new(),
        about: Typing::User { whose, field },
    })
}

/// What the account is allowed to do — or the line saying why it cannot be
/// changed.
///
/// The one row on this page that is a set of alternatives, and the one that can
/// be missing. An account that is the only administrator left does not get the
/// choice, because taking it away would leave a machine nobody can install
/// anything on, change the clock on, or make another account on — and it could
/// not be undone from here, since every one of those needs an administrator to
/// agree to it. The row still appears, saying what the account is and why that
/// is fixed: a row that vanished would answer "where has the account type gone"
/// with silence.
fn account_type_row(
    whose: crate::users::Whose,
    person: Option<&crate::users::Person>,
    admin: bool,
    listing: &crate::users::Listing,
) -> Entry {
    let sole = person.is_some_and(|person| listing.only_admin(person.uid));
    if sole {
        return reading_marked(
            "Account type",
            "Administrator — and the only one on this machine, so this cannot \
             be changed. Make another administrator first.",
            icons::SETTING_ACCOUNT_TYPE,
        );
    }
    let _ = whose;
    folder(
        "Account type",
        match admin {
            true => "Administrator",
            false => "Standard",
        },
        icons::SETTING_ACCOUNT_TYPE,
        vec![
            value(
                "Standard",
                Some("Can use this machine, and change their own settings"),
                !admin,
                Setting::User(UserValue::Admin(false)),
            ),
            value(
                "Administrator",
                Some("Can also install software and manage the other accounts"),
                admin,
                Setting::User(UserValue::Admin(true)),
            ),
        ],
    )
}

/// The avatar, chosen by walking to it.
///
/// A subcategory whose column is the disk itself, exactly as the wallpaper's own
/// picker is and for the same reason: the value is a file, there is no list of
/// files to put in a column beforehand, and standing on one is how somebody sees
/// which it is. See [`crate::files::Shows::Portrait`], which is what narrows the
/// walk to pictures.
///
/// *Avatar* rather than *Picture*, and the word is doing work. Every other
/// picture this shell offers to choose is a picture of *something* — a
/// wallpaper, a game's cover, the still behind a shelf. This one is a picture of
/// a **person**, it is the thing every row on the page above is drawn with, and
/// it is the word a console user already has for it.
///
/// Taking one off lives *inside* this column rather than beside it — see
/// [`no_avatar_row`]. It used to be a sibling row on the form, which put two
/// rows on every form about one value and left the form asking two questions
/// where there is one: what is this person's avatar, of which "none" is an
/// answer like any other.
fn avatar_row(picture: Option<&Path>) -> Entry {
    let comment = picture
        .and_then(|file| file.file_name())
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_string)
        .unwrap_or_else(|| "None — the plain mark".to_string());
    let Entry::Folder(mut inner) = folder("Avatar", &comment, icons::SETTING_AVATAR, Vec::new())
    else {
        unreachable!("folder builds a folder");
    };
    // The face itself on the row, once one has been chosen: the row is what the
    // account is going to look like, and saying so in a file name is saying it
    // in the one language a picture cannot be read in.
    inner.portrait = picture.map(Path::to_path_buf);
    // Read on the press that opens it rather than now — walking somebody's home
    // directory every time any setting anywhere changed is the cost this avoids.
    inner.place = Some(crate::files::Place::Volumes(crate::files::Shows::Portrait));
    Entry::Folder(inner)
}

/// The other answer at the head of that walk: no avatar at all.
///
/// It stands inside the picker rather than beside the row that opens it, because
/// it is an answer to the question the picker asks. A column of photographs asks
/// "which one", and "none of them" belongs in it — the same argument the
/// wallpaper's own Default row makes one column further out.
///
/// Over the list rather than in it, on the terms [`crate::apps::Entry::Pick`] is
/// — see [`crate::apps::Choice::over_the_list`]. The column opens on the first
/// disk below it, so a press of A on the way in cannot clear somebody's avatar
/// before they have read the row.
///
/// `None` where there is nothing to take off, which is a walk somebody opened
/// from a form with no avatar on it: a row offering to remove nothing is a row
/// that does nothing, and the column is one place where that would be pressed.
pub fn no_avatar_row() -> Option<Entry> {
    crate::users::form()?.picture?;
    let Entry::Choice(mut choice) = action(
        "Use no avatar",
        "Go back to the plain mark",
        icons::SETTING_PERSON,
        Setting::User(UserValue::DropPicture),
    ) else {
        unreachable!("action builds a choice");
    };
    choice.over_the_list = true;
    Some(Entry::Choice(choice))
}

/// The row at the foot of the form that hands it over.
///
/// Three shapes, and only one of them can be pressed. While something is in
/// flight it says so and does nothing: on this page "in flight" nearly always
/// means polkit is asking somebody for a password, which can take as long as
/// typing one takes, and a row that could be pressed again in the meantime would
/// ask the machine to make the same account twice.
///
/// Where the form is not yet a valid account it says what is missing, as a line
/// that cannot be pressed rather than a press that fails. That is the whole of
/// why the check is here as well as on each field's own panel: a field is
/// checked when somebody presses Set on it, and a form can be filled in by
/// pressing nothing at all — walking straight to the bottom and pressing once.
///
/// It wears the padlock, which is the mark of the panel that asks for a
/// password. That is what pressing it raises: the change goes to
/// `accounts-daemon`, the daemon asks polkit, and polkit asks this shell's own
/// agent. See [`crate::polkit`].
fn accept_row(
    whose: crate::users::Whose,
    person: Option<&crate::users::Person>,
    listing: &crate::users::Listing,
) -> Entry {
    let title = match whose {
        crate::users::Whose::New => "Accept and create",
        crate::users::Whose::Existing(_) => "Save changes",
    };
    if listing.working {
        return reading(
            title,
            "Waiting for permission to change the accounts on this machine",
        );
    }
    if let Some(fault) = crate::users::fault_in_form_for(whose) {
        return reading(title, fault);
    }
    let note = match person {
        Some(person) => format!("Save this to {}'s account", person.title()),
        None => "Make the account on this machine".to_string(),
    };
    // The shell's one tick, the same one a picker's answer row wears — see
    // [`crate::apps::Entry::Pick`], which is pressed to commit a walk exactly as
    // this is pressed to commit a form. Not the padlock it wore first: a padlock
    // is the thing that is *locked*, so a form whose last row was one was drawn
    // as the obstacle rather than as agreeing to it. And not a second tick of
    // its own, which is what it wore next — one drawing for one act.
    action(
        title,
        &note,
        icons::CHOSEN,
        Setting::User(UserValue::Accept),
    )
}

/// The row that takes an account off the machine, or the line saying why it
/// cannot be.
///
/// Two things stop it, and they are refused rather than hidden for the reason
/// the account type is: a row that vanished would answer "how do I remove this
/// account" with silence.
///
/// Somebody who is signed in cannot be removed. Their session is running, their
/// files are open, and the daemon would refuse it anyway — what this adds is the
/// reason, on the row, before the press.
///
/// Nor can the only administrator, on exactly the terms
/// [`crate::users::Listing::only_admin`] argues: it would leave a machine
/// nobody can administer, and nothing in this shell could put it right
/// afterwards.
fn removal_row(person: &crate::users::Person, listing: &crate::users::Listing) -> Entry {
    if person.you {
        return reading(
            "Remove account",
            "This is the account this session is running as; it cannot remove \
             itself",
        );
    }
    if person.here {
        return reading(
            "Remove account",
            "They are signed in on this machine. They have to sign out first.",
        );
    }
    if listing.only_admin(person.uid) {
        return reading(
            "Remove account",
            "The only administrator on this machine cannot be removed. Make \
             another administrator first.",
        );
    }
    action(
        "Remove account",
        // What the press costs, on the row: there is a panel between this and
        // the act, and it is the panel that asks about the files — but the row
        // has to say what it is before it is pressed all the same.
        "Take this account off the machine",
        icons::UNINSTALL,
        Setting::User(UserValue::Remove(person.uid)),
    )
}

fn setting(display: &'static str, value: DisplayValue) -> Setting {
    Setting::Display { display, value }
}

fn folder(title: &str, comment: &str, icon: &str, entries: Vec<Entry>) -> Entry {
    Entry::Folder(Folder {
        title: title.to_string(),
        comment: Some(comment.to_string()),
        icon: Some(icon.to_string()),
        entries,
        // The settings tree is written here, in full, on every rebuild. Nothing
        // in it comes off the disk, so there is no place for it to come back to.
        place: None,
        chosen: false,
        over_the_list: false,
        person: None,
        portrait: None,
    })
}

/// A subcategory that is also one of a set of answers, and is the one in force.
///
/// The tick and the way further in on the same row. There is exactly one of
/// these in the tree — the wireless network a radio is on — and the argument
/// for it is in [`crate::apps::Folder::chosen`]: that row is an answer to the
/// question its column asks *and* the only place the values belonging to that
/// answer can honestly hang.
fn chosen_folder(title: &str, comment: &str, icon: &str, entries: Vec<Entry>) -> Entry {
    let Entry::Folder(mut inner) = folder(title, comment, icon, entries) else {
        unreachable!("folder builds a folder");
    };
    inner.chosen = true;
    Entry::Folder(inner)
}

/// One colour in a list of them: named, drawn in itself, and marked when it is
/// the one in force.
fn swatch(title: &str, colour: Color, chosen: bool, setting: Setting) -> Entry {
    Entry::Choice(Choice {
        title: title.to_string(),
        comment: None,
        icon: Some(icons::SWATCH.to_string()),
        swatch: Some(colour),
        material: None,
        chosen,
        acts: false,
        setting: Some(setting),
        over_the_list: false,
    })
}

/// One value in a list of them, where the value is not a colour.
///
/// The same bead a swatch is drawn on, and deliberately: it is the mark of
/// "one of these", and the shell has exactly one of those. Without a colour to
/// be tinted with, the row is drawn in the accent's deep shade like every
/// other icon in the bar, so a list of numbers reads as a list rather than as
/// five colourless swatches.
fn value(title: &str, comment: Option<&str>, chosen: bool, setting: Setting) -> Entry {
    drawn_value(title, comment, icons::SWATCH, chosen, setting)
}

/// The same, for a value that is a picture of something rather than a number.
///
/// The bead is the mark of "one of these", and it is the right mark for nearly
/// everything here: a brightness in cd/m² has no shape, and drawing one would
/// be inventing a picture of a number. An orientation does have a shape — it is
/// the only value in this tree that is one — so it is drawn instead.
fn drawn_value(
    title: &str,
    comment: Option<&str>,
    icon: &str,
    chosen: bool,
    setting: Setting,
) -> Entry {
    Entry::Choice(Choice {
        title: title.to_string(),
        comment: comment.map(str::to_string),
        icon: Some(icon.to_string()),
        swatch: None,
        material: None,
        chosen,
        acts: false,
        setting: Some(setting),
        over_the_list: false,
    })
}

/// One value in a list of them, where the value is a *material*.
///
/// Four rows in the tree, and they are the Theme page's own: the row is drawn
/// in the material it applies, so the two rows of a column can carry the same
/// drawing. See [`crate::apps::Choice::material`] for the argument and
/// [`material_row`] for what it looks like on the page.
fn material_value(
    title: &str,
    comment: &str,
    icon: &str,
    material: wallpaper::Style,
    chosen: bool,
    setting: Setting,
) -> Entry {
    let Entry::Choice(mut choice) = drawn_value(title, Some(comment), icon, chosen, setting) else {
        unreachable!("drawn_value builds a choice");
    };
    choice.material = Some(material);
    Entry::Choice(choice)
}

/// Something the shell can show but not change.
///
/// It carries no [`Setting`], so choosing it does nothing and no mark moves.
/// That is the whole point of the distinction: a row that describes something
/// true must not be one the user can un-choose.
fn reading(title: &str, note: &str) -> Entry {
    reading_marked(title, note, icons::SETTING_INFO)
}

/// The same, keeping the mark the row would have worn if it could be pressed.
///
/// For the rows that are not *explanations* but settings with nothing to be
/// done about them — a user name on an account somebody is signed in to, the
/// account type of the last administrator. The information mark says "here is
/// something to read"; these rows are still the field they always were, and
/// three of them wearing one mark is the failure this page has already been
/// through once. What says they cannot be pressed is that they cannot be
/// pressed, and the note that says why.
fn reading_marked(title: &str, note: &str, icon: &str) -> Entry {
    Entry::Choice(Choice {
        title: title.to_string(),
        comment: Some(note.to_string()),
        icon: Some(icon.to_string()),
        swatch: None,
        material: None,
        chosen: false,
        acts: false,
        setting: None,
        over_the_list: false,
    })
}

/// One row that does a thing, in a column whose other rows are answers.
///
/// The only shape in this tree that is not a value, a bar, a typed field or a
/// way further in — see [`crate::apps::Choice::acts`], which says why it takes
/// no mark and moves none. There are three of them, all under one wireless
/// network: leaving it, joining it again, and forgetting it.
///
/// It wears a drawing rather than the bead, because the bead means *one of
/// these* and this is not one of anything. The comment carries what pressing it
/// costs, which is the whole of the warning a row like this gets: there is no
/// panel between the press and the act, so what Forget removes has to be
/// legible from the row itself.
fn action(title: &str, comment: &str, icon: &str, setting: Setting) -> Entry {
    Entry::Choice(Choice {
        title: title.to_string(),
        comment: Some(comment.to_string()),
        icon: Some(icon.to_string()),
        swatch: None,
        material: None,
        chosen: false,
        acts: true,
        setting: Some(setting),
        over_the_list: false,
    })
}

/// Preview the setting under the cursor, or return to what was applied when
/// the cursor is no longer on a setting value.
///
/// This deliberately moves no mark in the catalogue and writes no file. The
/// highlighted row is an invitation to look; only [`apply`] is a decision.
///
/// Only the accent previews. The rest of the tree reconfigures a connector —
/// which on most displays means the picture cuts to black for a second while
/// the panel resynchronises — and a setting that blacked the screen out every
/// time the cursor passed over it would be unusable. Walking down that list is
/// an invitation to look at the *names*, and the value is not committed until
/// it is chosen.
///
/// The Start music switch does not preview either, and it is the one row here
/// that could have: turning music off is instant and costs nothing. What it
/// cannot do is turn back on *usefully*. The track is rebuilt from sample zero
/// every time it starts — see [`crate::sound`] — so a cursor walked from Off to
/// On and back would answer with the same four hundred milliseconds of fade-in
/// over and over, which is not what the setting sounds like. Choosing it is the
/// decision, and then it is heard as it really is.
///
/// Nor do the device rows, and they are the ones where previewing would do real
/// harm. Highlighting one would move every sound on the machine to it — the
/// film somebody is watching, the call they are on — and walking down a list of
/// four would do that four times. A user looking for the right output is
/// looking at the *names* first; the sound follows when they choose.
///
/// Nor do the network rows, where it would be worse still. Highlighting one
/// would take the machine off the network it is on and put it on the one the
/// cursor happened to be passing — mid-download, mid-call — and on a secured
/// network it would put a password panel up for a row nobody chose. A list of
/// networks is a list of *names* to walk down; the radio moves when a row is
/// pressed.
pub fn preview(setting: Option<Setting>) {
    match setting {
        Some(Setting::Accent(name)) => {
            if !theme::preview_accent(name) {
                tracing::warn!(accent = name, "no accent by that name");
            }
        }
        // The material lands whole rather than travelling, because there is no
        // halfway between a bead of water and the flat shape of one. Previewed
        // all the same, and these are the rows here that need previewing most:
        // the difference between the two values is the screen itself, and no
        // name for it would tell anybody what they are choosing.
        //
        // One half at a time, which is the point of splitting them: highlighting
        // Simple under Wallpaper must leave the marks exactly as they are, so
        // that what changes on screen is what the row is about and nothing else.
        Some(Setting::Style(part, name)) => {
            if !theme::preview_style(part, name) {
                tracing::warn!(part = part.title(), theme = name, "no theme by that name");
            }
        }
        // Highlighting a value the compositor or the sound server would have to
        // act on changes nothing; the accent goes back to what is applied, as
        // it does when the cursor leaves a list of values entirely.
        //
        // The application scale is one of those. It also never arrives here in
        // practice — it is set on a bar, and a bar is not a row the cursor
        // highlights — but it is a setting like any other and is answered like
        // one, so that the day something else offers it there is no arm missing.
        Some(
            Setting::Display { .. }
            | Setting::StartMusic(_)
            | Setting::BatteryPercent(_)
            | Setting::ButtonHints(_)
            // Nor a Steam row, and the two below it could not preview if they
            // wanted to: what they change is what happens the next time the
            // session starts and the next time a game ends. The integration
            // switch *could* be shown — it rebuilds the bar — and must not be:
            // a cursor walking past Off would take the user's library off the
            // screen and put it back, twice, on the way down a list of two.
            | Setting::Steam(_)
            | Setting::SoundDevice { .. }
            | Setting::Network(_)
            | Setting::Bluetooth(_)
            // Nor does an account form. Highlighting Administrator must not make
            // somebody one, and there is nowhere for a preview of it to show:
            // what a form row changes is a draft two rows further down the same
            // column, which is already on screen saying what it says.
            | Setting::User(_)
            // Neither emulator setting previews, and neither could: what they
            // change is a file an emulator reads when it starts, and the
            // emulator is not running while somebody is walking down this
            // list. Writing one on every highlight would be writing four
            // settings to get to the fifth.
            | Setting::CoreOption { .. }
            | Setting::Emulator { .. }
            | Setting::EmulatorArt
            | Setting::AppScale(_)
            // The floating window is the compositor's too, and there is a
            // second reason not to preview it: what a highlighted row there
            // would move is somebody's video, and a cursor walking down the
            // four corners would throw it round the screen four times.
            | Setting::PictureInPicture(_)
            // And the startup column, which has nothing to preview: what it
            // changes is where the next cursor is built, and a preview of that
            // would be the bar walking off under the hand of somebody reading
            // the list.
            | Setting::StartupCategory(_)
            // Nor does the screen the on-screen keyboard comes up on, and it is
            // the one row here where previewing would be *visible* and still
            // wrong: a cursor walked down a list of two screens would throw the
            // board from one to the other and back, and the board is what the
            // user is reading the list through.
            | Setting::KeyboardDisplay(_)
            // Nor the keyboard layout, and it is the one row on this page where
            // previewing would be actively dangerous: what a highlighted row
            // would change is what every key on the machine types, under the
            // hands of somebody walking a list — and on a board they may be
            // walking it *with*. The mark moves when the row is pressed.
            | Setting::KeyboardLayout(_)
            // Nor do the mouse rows, and three of them are the same case as the
            // display settings: they reconfigure a device the compositor holds.
            // The fourth is the reason the whole list refuses — the pointer is
            // what the user is *reading this list with*, and a cursor walked
            // down a speed bar would change how fast the walk itself moves,
            // under the hand doing the walking. Two of the three are set on a
            // bar besides, which is not a row a cursor highlights.
            | Setting::Pointer(_),
        )
        | None => {
            theme::restore_accent();
            // And the material, for the same reason and in the same breath: a
            // cursor that walked onto Simple and off again has to leave the
            // shell in the theme the user is actually using.
            theme::restore_style();
        }
    }
}

/// Put a setting into force, and write it down. `false` if it names something
/// the shell does not have, which nothing built from [`column`] can.
///
/// The accent needs nothing told: every colour in the shell is read from
/// [`theme::theme`] as it is drawn, once a frame, the wallpaper's own uniforms
/// included. The Display settings do need telling, but not from here — the
/// caller sends whatever [`hdr_for`] now returns over the session protocol,
/// which keeps this module free of any Wayland connection and lets it be
/// tested without one. A sound device needs telling too, and for the same
/// reason is told by the caller: it goes to the sound server, and a module that
/// spawned `pactl` could not be tested on a machine that has none.
pub fn apply(setting: Setting) -> bool {
    apply_with(setting, save)
}

/// The whole mode one half of one asks for, given what that display is set to
/// and what it is doing.
///
/// The page asks two questions and the connector takes one answer, so each row
/// has to fill in the other half. What stands in for it is what the display is
/// already set to, and failing that what it is running — a display nobody has
/// configured still has a size to hang a rate on.
///
/// The size is the half that decides. A rate is offered under one size and
/// keeps it, always — a row on a page of 1440p rates that moved the display to
/// 1080p would undo the Resolution page from underneath it. Only a size moves
/// the other half, and only as far as it must: it takes the current rate along
/// where the new size carries it, and asks for the fastest that size has where
/// it does not. Neither ever asks for a combination the connector has not
/// listed.
///
/// `None` only when nothing anywhere knows what this display is, which is a
/// display that has gone away between the row being drawn and being chosen.
fn mode_from(display: &str, value: DisplayValue) -> Option<Mode> {
    let offered = offered_by(display);
    match value {
        DisplayValue::Resolution(resolution) => {
            let carried = wanted(display, &offered)
                .map(|mode| mode.refresh)
                .unwrap_or(0);
            let carries = rates_at(&offered, resolution)
                .iter()
                .any(|(refresh, _)| *refresh == carried);
            Some(Mode {
                resolution,
                // 0 is the compositor's "the fastest of that size", which is
                // what a size with no rate to carry over asks for.
                refresh: if carries { carried } else { 0 },
            })
        }
        // The size this rate was listed under, which is the one the page was
        // showing when the row was drawn.
        DisplayValue::RefreshRate(refresh) => Some(Mode {
            resolution: size_shown(display, &offered)?,
            refresh,
        }),
        _ => None,
    }
}

/// The whole arrangement one press asks for: the order in force, with this
/// screen and whichever screen was standing at `place` exchanged.
///
/// `None` when this screen is not in the arrangement at all, or when there is
/// no such place to move to — both of which are a display that went away
/// between the row being drawn and the row being chosen.
fn order_after_moving(display: &str, place: u32) -> Option<Vec<String>> {
    let mut order = arrangement();
    let from = order.iter().position(|name| name == display)?;
    let to = usize::try_from(place).ok().filter(|to| *to < order.len())?;
    order.swap(from, to);
    Some(order)
}

fn apply_with(setting: Setting, persist: impl FnOnce(&Stored)) -> bool {
    match setting {
        Setting::Accent(name) => {
            if !theme::commit_accent(name) {
                tracing::warn!(accent = name, "no accent by that name");
                return false;
            }
            tracing::info!(accent = name, "accent");
        }
        // Nothing to tell anybody: the wallpaper and every glyph are drawn from
        // this once a frame, so the frame the row was pressed on is the frame
        // the shell changes material in.
        Setting::Style(part, name) => {
            if !theme::commit_style(part, name) {
                tracing::warn!(part = part.title(), theme = name, "no theme by that name");
                return false;
            }
            tracing::info!(part = part.title(), theme = name, "theme");
        }
        // Nothing to tell anybody either, for the opposite reason to the
        // accent's: the shell asks this of itself once a frame — see
        // [`crate::sound::Sounds::sync_music`] — so the answer is acted on by
        // the loop that was about to draw the frame this row was chosen in.
        // Both of these are written straight into RetroArch's own files, and
        // neither is remembered here: the emulator is what reads them, and a
        // second copy in this shell's settings would be a second answer to a
        // question with one.
        Setting::CoreOption { core, key, value } => {
            if !crate::retroarch::set_core_option(core, key, value) {
                tracing::warn!(core, key, value, "the core setting could not be written");
                return false;
            }
        }
        Setting::Emulator { key, value } => {
            if !crate::retroarch::set_setting(key, value) {
                tracing::warn!(key, value, "the RetroArch setting could not be written");
                return false;
            }
        }
        // Nothing is written down and nothing is remembered: what the press
        // means is a helper being run, and the shell is what runs it. See
        // [`Setting::EmulatorArt`].
        Setting::EmulatorArt => {}
        Setting::StartMusic(playing) => {
            *START_MUSIC.lock().unwrap() = playing;
            tracing::info!(playing, "Start music");
        }
        // Nothing to tell anybody either, and for the plainest reason of the
        // three: the corner is laid out from this value every time it is drawn,
        // so the frame this row was pressed on is the frame the figures appear
        // or go. See [`crate::ui::Corner::percent`].
        Setting::BatteryPercent(written) => {
            *BATTERY_PERCENT.lock().unwrap() = written;
            tracing::info!(written, "battery percentage");
        }
        // Nothing to tell anybody either, and for the corner's own reason:
        // every legend in the shell is laid out from this value each time its
        // screen is drawn, so the frame this row was pressed on is the frame
        // they appear or go. See [`crate::ui::Legend`].
        //
        // Nothing to tell an *application* either, although they read it too:
        // the toolkit takes it out of `shell.toml`, which [`save`] writes as
        // this row is pressed, and a program already running reads it on its
        // own terms rather than being interrupted to be told.
        Setting::ButtonHints(shown) => {
            *BUTTON_HINTS.lock().unwrap() = shown;
            tracing::info!(shown, "button hints");
        }
        // Three switches about one program, and this module's whole part in
        // them is remembering which way each is thrown. What has to *happen* —
        // a worker started or stopped, a bar built again, a client asked to
        // shut down — is the shell's, on the terms the network and the sound
        // device are under. See `Shell::carry_out_steam`.
        //
        // The flag is deliberately not consulted here. A session started with
        // `--no-steam` offers no row to press, so nothing can arrive; and if
        // one ever did, the file should still record the answer the user gave
        // rather than the one this session was able to act on.
        Setting::Steam(SteamValue::Integration(on)) => {
            *STEAM_INTEGRATION.lock().unwrap() = on;
            tracing::info!(on, "the Steam integration");
        }
        Setting::Steam(SteamValue::AtStartup(on)) => {
            *STEAM_AT_STARTUP.lock().unwrap() = on;
            tracing::info!(on, "Steam is started with the shell");
        }
        Setting::Steam(SteamValue::AfterAGame(on)) => {
            *STEAM_AFTER_A_GAME.lock().unwrap() = on;
            tracing::info!(on, "Steam is left running after a game");
        }
        // Nothing is written down here, and that is the whole of this arm.
        // The value lives in Valve's own configuration rather than in this
        // shell's file — see [`SteamValue::OtherTitles`] — so the press is
        // carried out by telling Steam, and what the row draws afterwards is
        // what Steam says back. `Shell::carry_out_steam` is where it is told.
        Setting::Steam(SteamValue::OtherTitles(tool)) => {
            tracing::info!(
                tool = tool.unwrap_or("none"),
                "what runs the games Valve has not verified"
            );
        }
        // Nothing to tell anybody, and nothing to carry out. What this changes
        // is where the *next* cursor is built standing — see
        // [`crate::model::Cursor::for_model`] — so the press moves the mark,
        // this writes it down, and the bar under the user's hands does not
        // move. That is the setting working, not the setting failing.
        Setting::StartupCategory(id) => {
            *STARTUP_CATEGORY.lock().unwrap() = Some(id.to_string());
            tracing::info!(column = id, "the column the start screen opens on");
        }
        // Nothing to tell anybody either, and for the plainest reason: whichever
        // display is about to draw a frame asks this of itself as it draws —
        // see `Shell::keyboard_panel` — so the board is on the chosen screen the
        // next time it comes up, and a board already on screen walks across on
        // the frame the row was pressed.
        //
        // Not checked against the screens this session has. A connector that is
        // not plugged in is not a mistake in the press any more than it is a
        // mistake in the file: the page offers exactly one such row, it offers
        // it because the setting already named it, and choosing it again has to
        // go on meaning what it meant. See [`KEYBOARD_DISPLAY`].
        // Recorded here and carried out by the compositor, on the terms the
        // application scale is under: only the compositor holds the seat, so
        // only it can tell libinput anything — and only it draws the cursor.
        // One field of the four at a time, because the page asks four questions
        // and the request carries all four; see [`pointer`], which is what the
        // caller sends.
        Setting::Pointer(value) => {
            let mut held = POINTER.lock().unwrap();
            match value {
                PointerValue::Speed(speed) => {
                    held.speed = speed.clamp(SLOWEST_POINTER, FASTEST_POINTER)
                }
                PointerValue::Size(size) => held.size = size,
                PointerValue::Scroll(scroll) => {
                    held.scroll = scroll.clamp(SLOWEST_SCROLL, FASTEST_SCROLL)
                }
                PointerValue::Natural(natural) => held.natural = natural,
            }
            tracing::info!(
                speed = held.speed,
                size = held.size,
                scroll = held.scroll,
                natural = held.natural,
                "the pointing devices"
            );
        }
        // Recorded here and carried out by the compositor, which the caller
        // tells: the seat is the compositor's, and a shell that held a keyboard
        // would be taking the keys from the application being typed into.
        //
        // Written down as it was given, not as this machine's registry
        // describes it. A layout is a pair of xkb names and the registry is a
        // file that changes with a package; a setting rewritten into whatever
        // the current xkeyboard-config calls it would be a setting that moved
        // when nobody asked.
        Setting::KeyboardLayout(key) => {
            *KEYBOARD_LAYOUT.lock().unwrap() = Some(key.to_string());
            tracing::info!(layout = key, "the keyboard layout");
        }
        Setting::KeyboardDisplay(screen) => {
            *KEYBOARD_DISPLAY.lock().unwrap() = screen.map(str::to_string);
            tracing::info!(
                screen = screen.unwrap_or("the one being driven"),
                "the screen the on-screen keyboard comes up on"
            );
        }
        // Recorded here and carried out by the compositor, which the caller
        // tells — the same division the Display settings are under, and for the
        // same reason: what an application is configured at is not the shell's
        // to do, and a module that held a Wayland connection could not be
        // tested without one.
        //
        // Clamped rather than refused, as the compositor clamps it: a bar built
        // from [`NATURAL_SCALE`] and [`LARGEST_SCALE`] cannot ask for anything
        // outside them, and a hand-edited file that does is answered with the
        // nearest size that means something.
        Setting::AppScale(percent) => {
            let percent = percent.clamp(NATURAL_SCALE, LARGEST_SCALE);
            *APP_SCALE.lock().unwrap() = percent;
            tracing::info!(percent, "application scale");
        }
        // Recorded here and carried out by the compositor, on the same terms as
        // the scale above: what a window's rectangle is belongs to the half of
        // the session that draws windows. One field of the three at a time,
        // because the page asks three questions and the request carries all
        // three — see [`picture_in_picture`], which is what the caller sends.
        Setting::PictureInPicture(value) => {
            let mut held = PICTURE_IN_PICTURE.lock().unwrap();
            match value {
                PipValue::Floating(floating) => held.floating = floating,
                PipValue::Size(size) => held.size = size,
                PipValue::Place(place) => held.place = place,
            }
            tracing::info!(
                floating = held.floating,
                size = held.size.key(),
                place = held.place.key(),
                "picture-in-picture"
            );
        }
        // The one row here that is neither carried out nor written down by this
        // module. It is passed to the sound server — the caller does that, the
        // way it sends the Display settings to the compositor, which is what
        // keeps this module free of both — and the server is also what
        // remembers it. A shell that kept its own copy would be a second
        // opinion about the machine's output device at every login, and the one
        // that lost would be whichever the user set last: a device chosen in
        // any other mixer would be quietly undone by this shell starting.
        //
        // So this returns without persisting. The mark on the row moves because
        // the listing itself moves — see [`crate::system::Quick::use_device`] —
        // rather than because anything here was recorded.
        //
        // The login screen is still told, though, and this is the one row where
        // that has to be said out loud rather than falling out of the file
        // being written: nothing is written, so the ordinary route — [`save`],
        // which ends in [`published`] — never runs. A login screen has to come
        // out of the same speakers as the session, and it has no sound server
        // of its own to ask which those are, so the only way it can know is if
        // somebody who does know says so at the moment it changes.
        Setting::SoundDevice { direction, id } => {
            tracing::info!(?direction, id, "sound device chosen");
            tell_the_login_screen(&LOGIN_SCREENS);
            return true;
        }
        // Bluetooth is the one page in this tree that is split down the middle,
        // and the line is drawn where BlueZ's knowledge ends. What is paired,
        // what it is called, whether it can be found: BlueZ's, handed to the
        // caller, written nowhere here. Which of two radios the user means and
        // what to do at startup: BlueZ has no opinion about either, so the
        // shell keeps them and they fall through to the file.
        Setting::Bluetooth(BluetoothValue::Use { address }) => {
            *BLUETOOTH_CONTROLLER.lock().unwrap() = Some(address.to_string());
            tracing::info!(address, "Bluetooth controller");
        }
        Setting::Bluetooth(BluetoothValue::Startup(startup)) => {
            *BLUETOOTH_STARTUP.lock().unwrap() = startup;
            tracing::info!(?startup, "Bluetooth at startup");
        }
        // The other row here that is neither carried out nor written down, and
        // for the same reason: `NetworkManager` is what does it and what
        // remembers it. The caller hands it over — see [`crate::network`] —
        // which is what keeps this module free of a D-Bus connection, exactly
        // as it is free of a Wayland one.
        //
        // The login screen is not told, and that is the one thing this row does
        // differently from the sound device above it. A login screen has to come
        // out of the same speakers as the session because nothing else could
        // tell it which; it does not have to be told about the network, because
        // the network manager it would ask is the same daemon this session just
        // spoke to and it is running before either of them.
        Setting::Network(value) => {
            tracing::info!(?value, "network");
            return true;
        }
        // And an account form, on the same terms once more — except that three
        // of its four presses do not reach a daemon at all. They write into a
        // draft, and the draft is `crate::users`'s. Nothing here is remembered
        // between sessions: an account is a fact about the machine, kept by the
        // machine, and a copy of it in this shell's settings file would be a
        // second opinion about who may log in.
        Setting::User(value) => {
            tracing::info!(?value, "account form");
            return true;
        }
        // And the rest of Bluetooth, on exactly the terms the network is under:
        // BlueZ carries it out and BlueZ remembers it, the caller hands it over
        // — see [`crate::bluetooth`] — and nothing of it goes into the settings
        // file. The login screen is not told, for the reason the network does
        // not tell it: `bluetoothd` is running before either of them and is the
        // same daemon a login screen would ask.
        Setting::Bluetooth(value) => {
            tracing::info!(?value, "bluetooth");
            return true;
        }
        // Bound as `screen`, not `display`: tracing's macros pull their own
        // `display` into scope, and a field whose value is named that resolves
        // to the formatting helper instead of to this string.
        Setting::Display {
            display: screen,
            value,
        } => {
            // The mode is filed on its own rather than among the HDR settings
            // it sits beside on the page: a screen given a mode has not
            // thereby been given a colour pipeline, and reaching for those
            // would put a section in the file nobody asked for.
            match value {
                DisplayValue::Resolution(_) | DisplayValue::RefreshRate(_) => {
                    let Some(mode) = mode_from(screen, value) else {
                        tracing::warn!(screen, ?value, "nothing is known about this display");
                        return false;
                    };
                    MODE.lock().unwrap().insert(screen.to_string(), mode);
                }
                // Filed on its own for the same reason the mode is: a screen
                // that has been turned has not been given a mode or a colour
                // pipeline, and writing one it never asked for would put a
                // display's whole picture in the file the first time somebody
                // stood one on its side.
                DisplayValue::Orientation(turn) => {
                    TURN.lock().unwrap().insert(screen.to_string(), turn);
                }
                // And the arrangement on its own again, for the same reason —
                // but written as a whole rather than as one screen's entry,
                // because that is what it is. Moving a display moves the one it
                // trades with, and a file that recorded only the screen that
                // was pressed would be describing an order no two screens
                // agree on.
                //
                // Worked out against what the compositor last reported rather
                // than against what was last asked for: that is what the page
                // is showing, so it is what the press was aimed at. The
                // compositor answers a move within the frame — it is a
                // relayout, not a modeset — so the two cannot drift apart the
                // way a resolution can.
                DisplayValue::Place(place) => {
                    let Some(order) = order_after_moving(screen, place) else {
                        tracing::warn!(screen, place, "this display has no place to move from");
                        return false;
                    };
                    let mut held = PLACE.lock().unwrap();
                    for (place, name) in order.into_iter().enumerate() {
                        held.insert(name, place as u32);
                    }
                }
                // And the night light on its own again, for the third time and
                // the same reason: a screen somebody warmed has not thereby
                // been given a mode, a turn or a colour pipeline.
                //
                // All five rows write to one entry, because the page asks five
                // questions about one filter and the answer to "is it on now"
                // is made of all of them.
                DisplayValue::NightLight(_)
                | DisplayValue::NightLightTemperature(_)
                | DisplayValue::NightLightSchedule(_)
                | DisplayValue::NightLightFrom(_)
                | DisplayValue::NightLightUntil(_) => {
                    let mut held = NIGHT.lock().unwrap();
                    let night = held.entry(screen.to_string()).or_default();
                    match value {
                        DisplayValue::NightLight(on) => night.enabled = on,
                        // Clamped rather than refused, as the white level is:
                        // the nearest temperature that means something is a
                        // far better way to report a number out of range than
                        // a display left at some unrelated colour. To the
                        // *bar's* range, not the compositor's — a press cannot
                        // ask for a temperature the bar has no room for.
                        DisplayValue::NightLightTemperature(kelvin) => {
                            night.temperature = kelvin.clamp(WARMEST_ON_THE_BAR, NEUTRAL_KELVIN)
                        }
                        // The hours are kept across a change of schedule, so
                        // asking for them again gives back the evening that was
                        // there rather than one this shell invented.
                        DisplayValue::NightLightSchedule(schedule) => night.schedule = schedule,
                        DisplayValue::NightLightFrom(hour) => {
                            night.from = hour.min(23);
                            // The two may not meet: a window that ends where
                            // it begins is neither a whole day nor none of
                            // one. The Until page leaves the starting hour
                            // out, so this can only be reached by choosing a
                            // From that lands on the existing Until — and the
                            // answer to that is to move the other end, not to
                            // refuse the press.
                            if night.until == night.from {
                                night.until = (night.from + 1) % 24;
                            }
                        }
                        DisplayValue::NightLightUntil(hour) => {
                            let hour = hour.min(23);
                            if hour != night.from {
                                night.until = hour;
                            }
                        }
                        // Taken by the arms around this one.
                        _ => {}
                    }
                }
                // And the OLED protection on its own again, for the fourth
                // time and the same reason: a screen somebody asked to rest
                // has not thereby been given a mode, a turn, a warmth or a
                // colour pipeline.
                DisplayValue::OledProtection(on) => {
                    OLED.lock().unwrap().insert(screen.to_string(), on);
                }
                _ => {
                    let mut held = HDR.lock().unwrap();
                    let inherited = *INHERITED.lock().unwrap();
                    let settings = held.entry(screen.to_string()).or_insert(inherited);
                    match value {
                        DisplayValue::Hdr(enabled) => settings.enabled = enabled,
                        // Clamped rather than refused: a white level of zero is
                        // a dark display, which is a far worse way to report a
                        // bad value.
                        DisplayValue::SdrBrightness(nits) => settings.sdr_brightness = nits.max(1),
                        DisplayValue::SrgbIntensity(percent) => {
                            settings.srgb_intensity = percent.min(100)
                        }
                        DisplayValue::PeakBrightness(nits) => settings.peak_brightness = nits,
                        // Taken by the arms above; the compiler cannot see it.
                        DisplayValue::Resolution(_)
                        | DisplayValue::RefreshRate(_)
                        | DisplayValue::Orientation(_)
                        | DisplayValue::Place(_)
                        | DisplayValue::NightLight(_)
                        | DisplayValue::NightLightTemperature(_)
                        | DisplayValue::NightLightSchedule(_)
                        | DisplayValue::NightLightFrom(_)
                        | DisplayValue::NightLightUntil(_)
                        | DisplayValue::OledProtection(_) => {}
                    }
                }
            }
            tracing::info!(screen, ?value, "display setting");
        }
    }
    persist(&stored());
    true
}

/// Read the settings back, before anything is drawn or assembled.
///
/// A missing file is the ordinary first run. An unreadable or unrecognised one
/// is reported and then left alone: refusing to start a session over a colour,
/// or over a brightness, would be a far worse failure than coming up violet in
/// SDR.
pub fn load() {
    let Some(path) = settings_path() else {
        tracing::debug!("no config directory; the shell's settings are not persisted");
        return;
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "could not read the shell settings");
            return;
        }
    };
    let stored: Stored = match toml::from_str(&raw) {
        Ok(stored) => stored,
        Err(err) => {
            tracing::warn!(%err, path = %path.display(), "ignoring unreadable shell settings");
            return;
        }
    };
    if let Some(accent) = &stored.accent {
        if theme::set_accent(accent) {
            tracing::info!(accent, path = %path.display(), "read the shell settings");
        } else {
            tracing::warn!(
                accent,
                "the settings name an accent this shell does not have"
            );
        }
    }
    adopt_theme(&stored);
    adopt(stored);
}

/// Put the two halves of the Theme setting into force from a parsed file.
///
/// Each half its own key, and the key they both used to share where a half has
/// nothing of its own: a file written before the setting was split says one
/// thing about the whole shell, and it meant it about both. Split from
/// [`load`] so that can be exercised without a file, which is the only way to
/// hold the fallback without writing into the developer's own home directory.
fn adopt_theme(stored: &Stored) {
    // The file first, because the material is what says whether it is drawn and
    // a Custom wallpaper with nothing to draw is the one combination this must
    // not put into force. A path that names nothing is dropped here rather than
    // further down: `Custom` then falls back to the shell's own scene by the
    // same route a file that turns out to be undecodable does, and the Settings
    // row goes back to saying what it can honestly offer.
    match stored.wallpaper_file.as_ref().map(PathBuf::from) {
        Some(file) if file.is_file() => *CUSTOM_WALLPAPER.lock().unwrap() = Some(file),
        Some(file) => {
            tracing::warn!(
                file = %file.display(),
                "the wallpaper this shell was set to is not there, so it draws its own"
            );
        }
        None => {}
    }

    for (part, named) in [
        (theme::Part::Wallpaper, &stored.theme_wallpaper),
        (theme::Part::Icons, &stored.theme_icons),
    ] {
        let Some(named) = named.as_ref().or(stored.theme.as_ref()) else {
            continue;
        };
        // A machine whose file has gone is put back to the shell's own scene,
        // rather than left set to a picture it has not got. The key itself is
        // untouched — see [`Stored::wallpaper_file`] — so a drive plugged back
        // in tomorrow brings the wallpaper back with it.
        if part == theme::Part::Wallpaper
            && named == wallpaper::CUSTOM
            && custom_wallpaper().is_none()
        {
            continue;
        }
        if !theme::set_style(part, named) {
            // Named by the key rather than by the row, because this is about
            // what is in the file and the reader is looking at the file.
            tracing::warn!(
                key = part.key(),
                theme = named,
                "the settings name a theme this shell does not have"
            );
        }
    }
}

/// Take the display settings out of a parsed file.
///
/// Split from [`load`] so the file format can be exercised without one.
fn adopt(stored: Stored) {
    *MEDIA_SORT.lock().unwrap() = stored.media_sort;
    *STEAM_SORT.lock().unwrap() = stored.steam_sort;
    // Whatever it says, without asking the disk whether the folder is still
    // there. A collection on a drive that is not plugged in this morning is
    // still where the user said it was, and the row that says so is where they
    // would go to change it — see [`crate::retroarch`], which reads the folder
    // and says what it found.
    *ROMS_FOLDER.lock().unwrap() = stored.retroarch_roms.as_deref().map(PathBuf::from);

    // Before any display's section is read: a night light following the sun is
    // only kept where there is a sun to follow, and this is what decides that.
    // Half a coordinate is not a place, and one off the earth is not one
    // either — both are dropped with a word rather than believed.
    let written = stored
        .night_light_latitude
        .zip(stored.night_light_longitude);
    match written.map(|(latitude, longitude)| {
        (
            crate::sun::Location::exact(latitude, longitude),
            latitude,
            longitude,
        )
    }) {
        Some((Some(at), ..)) => crate::sun::set_location(Some(at)),
        Some((None, latitude, longitude)) => {
            tracing::warn!(latitude, longitude, "ignoring a location that is not one");
            crate::sun::set_location(None);
        }
        None => {
            if stored.night_light_latitude.is_some() || stored.night_light_longitude.is_some() {
                tracing::warn!("a location needs both a latitude and a longitude");
            }
            crate::sun::set_location(None);
        }
    }

    // A hand-edited level outside the range the row can reach is clamped
    // rather than refused, for the reason a mistyped mode is dropped rather
    // than refused: this is a file the user is entitled to open, and one silly
    // number in it must not take the accent down with it.
    {
        let mut sound = SOUND.lock().unwrap();
        if let Some(value) = stored.sound_volume {
            sound.value = value.clamp(0.0, 1.0);
        }
        if let Some(muted) = stored.sound_muted {
            sound.muted = muted;
        }
    }
    // A file that says nothing about it leaves the music where the session has
    // it, which on the ordinary first run is playing.
    if let Some(playing) = stored.start_music {
        *START_MUSIC.lock().unwrap() = playing;
    }
    // And the same for the switch that decides whether anything may interrupt.
    if let Some(quiet) = stored.do_not_disturb {
        *DO_NOT_DISTURB.lock().unwrap() = quiet;
    }
    // A file that says nothing about the battery's figures leaves them off,
    // which is where a session that has never been asked has them. Read on
    // every machine and not only on one with a battery: a laptop's settings
    // file opened on a desktop and carried back must not have lost the switch
    // in between.
    if let Some(written) = stored.battery_percent {
        *BATTERY_PERCENT.lock().unwrap() = written;
    }
    // And a file that says nothing about the hidden names leaves them hidden,
    // which is where a session that has never been asked has them.
    if let Some(written) = stored.show_hidden {
        *SHOW_HIDDEN.lock().unwrap() = written;
    }
    // And one that says nothing about the button hints leaves them on, which is
    // where a session that has never been asked has them — including every
    // session written by a shell from before this setting existed. See
    // [`button_hints`], where the default is argued.
    if let Some(shown) = stored.button_hints {
        *BUTTON_HINTS.lock().unwrap() = shown;
    }
    // And a file that says nothing about Steam leaves the integration on, the
    // client unstarted until a game is pressed, and a client that has been
    // started running afterwards — which is where a session that has never been
    // asked has all three, and is what every file written before this page
    // existed says. See [`SteamValue`], where each default is argued.
    if let Some(on) = stored.steam_integration {
        *STEAM_INTEGRATION.lock().unwrap() = on;
    }
    if let Some(on) = stored.steam_at_startup {
        *STEAM_AT_STARTUP.lock().unwrap() = on;
    }
    if let Some(on) = stored.steam_after_a_game {
        *STEAM_AFTER_A_GAME.lock().unwrap() = on;
    }
    // And one that says nothing about the startup column leaves it on Games,
    // which is where a session that has never been asked has it — including
    // every session written by a shell from before this setting existed. The
    // name is not checked against the columns this machine has: the bar is not
    // built yet, and a column that is not there this session is not a mistake
    // in the file. See [`STARTUP_CATEGORY`].
    if let Some(column) = stored.startup_category.clone() {
        *STARTUP_CATEGORY.lock().unwrap() = Some(column);
    }
    // And one that says nothing about the on-screen keyboard's display leaves
    // it following the screen being driven, which is where a session that has
    // never been asked has it. The name is not checked against the screens this
    // session has, for the reason the column is not checked against the bar:
    // the displays have not been announced yet, and a monitor that is not
    // plugged in this morning is not a mistake in the file. See
    // [`KEYBOARD_DISPLAY`].
    *KEYBOARD_DISPLAY.lock().unwrap() = stored.keyboard_display.clone();
    // And one that says nothing about the keyboard layout leaves the
    // compositor's own config in force, which is the whole reason this is an
    // `Option` and not a layout name with a default. Not checked against this
    // machine's registry, for the reason the connector above is not checked
    // against its screens: xkeyboard-config is a package, and a layout it
    // stopped describing is not a mistake in the file.
    *KEYBOARD_LAYOUT.lock().unwrap() = stored.keyboard_layout.clone();
    // What every mouse on this machine does. Each half read on its own, so a
    // file that says three of the four leaves the fourth where a session that
    // has never been asked has it — including every file written by a shell
    // from before this page existed. Clamped rather than refused, as the sound
    // level is.
    {
        let mut held = POINTER.lock().unwrap();
        if let Some(speed) = stored.pointer_speed {
            held.speed = speed.clamp(SLOWEST_POINTER, FASTEST_POINTER);
        }
        if let Some(size) = stored.cursor_size {
            held.size = nearest_cursor_size(size);
        }
        if let Some(scroll) = stored.scroll_speed {
            held.scroll = scroll.clamp(SLOWEST_SCROLL, FASTEST_SCROLL);
        }
        if let Some(natural) = stored.natural_scroll {
            held.natural = natural;
        }
    }
    // Which radio the machine's Bluetooth is, what to do with it at startup,
    // and what it was doing last time. A hand-edited word this shell does not
    // know is dropped with a warning rather than refused, as a mistyped mode is:
    // the file is one the user is entitled to open.
    *BLUETOOTH_CONTROLLER.lock().unwrap() = stored.bluetooth_controller;
    if let Some(key) = stored.bluetooth_startup.as_deref() {
        match Startup::from_key(key) {
            Some(startup) => *BLUETOOTH_STARTUP.lock().unwrap() = startup,
            None => tracing::warn!(key, "ignoring an unknown Bluetooth startup setting"),
        }
    }
    if let Some(on) = stored.bluetooth_was_on {
        *BLUETOOTH_WAS_ON.lock().unwrap() = on;
    }
    // And for which control the last session saw in the user's hands, which is
    // the whole point of writing that one down: a file that says nothing leaves
    // it on the controller, because that is what a console is held with.
    if let Some(in_hand) = stored.controller_in_hand {
        *CONTROLLER_IN_HAND.lock().unwrap() = in_hand;
    }
    // How large applications are drawn. Clamped rather than refused, as a
    // hand-edited sound level is: this is a file the user is entitled to open,
    // and a smaller number than one to one has to come back as one to one —
    // which is also what the compositor would do with it, so the file and the
    // screen agree.
    if let Some(percent) = stored.application_scale {
        *APP_SCALE.lock().unwrap() = percent.clamp(NATURAL_SCALE, LARGEST_SCALE);
    }
    // And what the floating window is given. Each half is taken on its own, so
    // a file that names a size nobody has heard of still keeps the corner
    // beside it — and says so, rather than refusing the file: this is a text
    // file the user is entitled to open.
    {
        let mut held = PICTURE_IN_PICTURE.lock().unwrap();
        if let Some(floating) = stored.picture_in_picture {
            held.floating = floating;
        }
        if let Some(size) = stored.picture_in_picture_size.as_deref() {
            match pip::Size::from_key(size) {
                Some(size) => held.size = size,
                None => tracing::warn!(size, "ignoring a picture-in-picture size with no meaning"),
            }
        }
        if let Some(place) = stored.picture_in_picture_place.as_deref() {
            match pip::Place::from_key(place) {
                Some(place) => held.place = place,
                None => {
                    tracing::warn!(
                        place,
                        "ignoring a picture-in-picture corner with no meaning"
                    )
                }
            }
        }
    }

    // The flat keys a single-display version of this page wrote, which become
    // the starting point for every display the file says nothing about.
    let defaults = Hdr::default();
    *INHERITED.lock().unwrap() = Hdr {
        enabled: stored.hdr.unwrap_or(defaults.enabled),
        sdr_brightness: stored
            .hdr_sdr_brightness
            .unwrap_or(defaults.sdr_brightness)
            .max(1),
        srgb_intensity: stored
            .hdr_srgb_intensity
            .unwrap_or(defaults.srgb_intensity)
            .min(100),
        peak_brightness: stored
            .hdr_peak_brightness
            .unwrap_or(defaults.peak_brightness),
    };

    let inherited = *INHERITED.lock().unwrap();
    let mut held = HDR.lock().unwrap();
    let mut modes = MODE.lock().unwrap();
    let mut turns = TURN.lock().unwrap();
    let mut places = PLACE.lock().unwrap();
    let mut nights = NIGHT.lock().unwrap();
    let mut rests = OLED.lock().unwrap();
    held.clear();
    modes.clear();
    turns.clear();
    places.clear();
    nights.clear();
    rests.clear();
    for (name, display) in stored.display {
        // A line that is not a mode is dropped with a word about it rather
        // than refusing the file: this is a text file the user is entitled to
        // open, and a typo in one display's mode must not take the accent and
        // the HDR settings down with it.
        // Bound before the macro below: `display` inside one resolves to
        // tracing's own formatting helper rather than to this section.
        let written = display.mode.clone();
        match written.as_deref().map(Mode::from_config) {
            Some(Some(mode)) => {
                modes.insert(name.clone(), mode);
            }
            Some(None) => tracing::warn!(
                screen = %name,
                mode = written.unwrap_or_default(),
                "ignoring a mode that is not WIDTHxHEIGHT@REFRESH"
            ),
            None => {}
        }
        // The same again for the turn, and dropped the same way: a screen the
        // file names an orientation this shell does not have is left the way
        // the compositor brought it up.
        let written = display.transform.clone();
        match written.as_deref().map(Orientation::from_key) {
            Some(Some(turn)) => {
                turns.insert(name.clone(), turn);
            }
            Some(None) => tracing::warn!(
                screen = %name,
                transform = written.unwrap_or_default(),
                "ignoring an orientation this shell does not have"
            ),
            None => {}
        }
        // And the place, counted from one in the file and from zero here. A
        // place before the first one is not a place: it is dropped with a word
        // rather than read as the first, because a file that names the same
        // screen twice — once as `0` and once as `1` — would otherwise become
        // an order with two first screens.
        match display.order {
            Some(0) => tracing::warn!(
                screen = %name,
                "ignoring a place before the first one; the places are counted from one"
            ),
            Some(place) => {
                places.insert(name.clone(), place - 1);
            }
            None => {}
        }
        // And the night light, which is filed on its own again. A section
        // saying nothing about it leaves that display unwarmed rather than
        // pinned to a copy of the default, so a screen carrying only a mode
        // does not acquire a filter it never asked for.
        if display.night_light.is_some()
            || display.night_light_temperature.is_some()
            || display.night_light_schedule.is_some()
            || display.night_light_from.is_some()
            || display.night_light_until.is_some()
        {
            let fallback = NightLight::default();
            let hour = |written: Option<u8>, default: u8| match written {
                // Clamped rather than dropped, for the reason a hand-edited
                // level is: this is a file the user is entitled to open, and
                // one silly number in it must not take the rest down with it.
                Some(hour) => hour.min(23),
                None => default,
            };
            let from = hour(display.night_light_from, fallback.from);
            let until = hour(display.night_light_until, fallback.until);
            // A word this shell does not have is dropped the way an unknown
            // orientation is: the light keeps whatever the switch says, and
            // the file keeps its word for whoever wrote it.
            let written = display.night_light_schedule.clone();
            let schedule = match written.as_deref().map(Schedule::from_key) {
                Some(Some(schedule)) => schedule,
                Some(None) => {
                    tracing::warn!(
                        screen = %name,
                        schedule = written.unwrap_or_default(),
                        "ignoring a night light schedule this shell does not have"
                    );
                    fallback.schedule
                }
                None => fallback.schedule,
            };
            // Two hours that meet are neither a whole day nor none of one, and
            // there is no honest guess between them — so the window is dropped
            // with a word about it. The sun's own hours cannot collide, so this
            // is only ever about the two the file names.
            let schedule = match schedule == Schedule::Hours && from == until {
                true => {
                    tracing::warn!(
                        screen = %name,
                        hour = from,
                        "ignoring a night light window that ends where it begins"
                    );
                    Schedule::AllDay
                }
                false => schedule,
            };
            // And a machine that cannot say where it is cannot follow the sun.
            // Left as All day rather than as a schedule that would never come
            // on — which is also why the page does not offer it there.
            let schedule =
                match schedule == Schedule::SunsetToSunrise && crate::sun::location().is_none() {
                    true => {
                        tracing::warn!(
                            screen = %name,
                            "this machine names no place, so the night light cannot follow the sun"
                        );
                        Schedule::AllDay
                    }
                    false => schedule,
                };
            nights.insert(
                name.clone(),
                NightLight {
                    enabled: display.night_light.unwrap_or(fallback.enabled),
                    temperature: display
                        .night_light_temperature
                        .unwrap_or(fallback.temperature)
                        .clamp(WARMEST_ON_THE_BAR, NEUTRAL_KELVIN),
                    schedule,
                    from,
                    until,
                },
            );
        }
        // And whether this screen rests while another one is being used,
        // which is a plain switch and needs none of the care above it. A
        // section that says nothing about it leaves that display alone, which
        // is what the setting does when nobody has answered it.
        if let Some(rest) = display.oled_protection {
            rests.insert(name.clone(), rest);
        }
        // A section that says nothing about HDR leaves that display on the
        // inherited settings rather than being pinned to a copy of them —
        // which is what a section carrying only a mode is.
        if display.hdr.is_none()
            && display.hdr_sdr_brightness.is_none()
            && display.hdr_srgb_intensity.is_none()
            && display.hdr_peak_brightness.is_none()
        {
            continue;
        }
        held.insert(
            name,
            Hdr {
                enabled: display.hdr.unwrap_or(inherited.enabled),
                sdr_brightness: display
                    .hdr_sdr_brightness
                    .unwrap_or(inherited.sdr_brightness)
                    .max(1),
                srgb_intensity: display
                    .hdr_srgb_intensity
                    .unwrap_or(inherited.srgb_intensity)
                    .min(100),
                peak_brightness: display
                    .hdr_peak_brightness
                    .unwrap_or(inherited.peak_brightness),
            },
        );
    }
}

/// The file, as it is written.
///
/// Every field optional, because a file that has been hand-edited down to one
/// line is a file that says one thing and leaves the rest at their defaults.
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct Stored {
    accent: Option<String>,
    /// Which material each half of the shell draws itself in — `Default` or
    /// `Simple`, one answer for the picture behind everything and one for every
    /// mark on top of it. The display manager reads both keys as well as the
    /// accent, so that a login screen never arrives in a material the session is
    /// not using; the compositor reads the wallpaper's alone, because the frame
    /// it bridges the start of the session with is a wallpaper and nothing else.
    theme_wallpaper: Option<String>,
    theme_icons: Option<String>,
    /// What both of them were before they were two settings.
    ///
    /// Read, never written. One `theme` key said what the whole shell was made
    /// of, and a file left by that shell means it about both halves — so it is
    /// what each of them falls back to, and the first save afterwards replaces
    /// it with the pair. Kept rather than dropped because the alternative is a
    /// machine somebody deliberately set to `Simple` coming back up in the water
    /// after an update.
    theme: Option<String>,
    /// The picture or film standing behind everything, where `theme-wallpaper`
    /// is `Custom wallpaper`.
    ///
    /// The shell's own copy of what the user chose, under
    /// `$XDG_DATA_HOME/linexinbar` — not the file they pressed. See
    /// [`crate::paper::keep`]: a setting that named somebody's Downloads folder
    /// would be a wallpaper that disappeared the next time they tidied it.
    ///
    /// Written whatever the theme says, so that a machine stood down to Simple
    /// for a while has its picture back when it is asked for. Read by this shell
    /// alone: the compositor's bridge frame and the login screen both draw the
    /// shell's own scene here — see [`wallpaper::Style::analytic`] — because
    /// neither of them is in a position to open a file under somebody's home.
    ///
    /// A file that is not there when the session starts is not an error and does
    /// not clear the key: the shell draws its own wallpaper for that session and
    /// says so in the log. A drive that was not plugged in this morning is the
    /// case that rule is for.
    wallpaper_file: Option<String>,
    /// The folder somebody's ROMs are in — see [`ROMS_FOLDER`]. Read by this
    /// shell alone, and only where the `lxb-retroarch` package is installed;
    /// on every other machine it is a line in the file that nothing reads,
    /// which is the honest way for a setting belonging to an optional package
    /// to be remembered across *that package* being uninstalled and put back.
    ///
    /// Removing RetroArch itself is a different act and does clear it — see
    /// [`forget_roms_folder`]. That is somebody saying "take this off my
    /// machine", and a shell that came back knowing where their games were
    /// would not have taken it off.
    retroarch_roms: Option<String>,
    /// What a display with no section of its own is set to.
    ///
    /// These four are where the first, single-display version of this page
    /// wrote *the* HDR settings, so a file left by that version is read as a
    /// statement about every screen — which is what it was. They keep being
    /// written for the same reason in reverse: once a screen is given a
    /// section, the rest must not quietly fall back to the shell's defaults at
    /// the next startup because the file stopped saying otherwise. A display
    /// plugged in for the first time gets these too.
    hdr: Option<bool>,
    hdr_sdr_brightness: Option<u16>,
    hdr_srgb_intensity: Option<u8>,
    hdr_peak_brightness: Option<u16>,
    /// How loud the shell's effects and Start music are, 0 to 1, and whether
    /// they are silenced. The machine's volume is deliberately not here: the
    /// sound server remembers that one, and a shell that wrote it down as well
    /// would be a second opinion about it at every login.
    sound_volume: Option<f32>,
    sound_muted: Option<bool>,
    /// Whether the Start screen plays its background music. Beside the two
    /// above because it is the same part of the shell, and separate from them
    /// because it is a different question: those say how loud everything the
    /// shell plays is, and this says whether one of the things it plays exists.
    start_music: Option<bool>,
    /// Whether the guide's do-not-disturb tile is on: announcements filed
    /// without a bubble and without a chime. Session-wide, like the three keys
    /// above it and unlike anything in `apps.toml`.
    do_not_disturb: Option<bool>,
    /// Whether this shell writes what its buttons do, wherever it has room to.
    /// Session-wide, like the switches around it, and written on every machine:
    /// a console handed to somebody else is the case this exists for, and it
    /// must not come back on because the shell was restarted.
    ///
    /// The one key here an *application* reads as well. Anything built on
    /// lxb-toolkit takes it out of this file for its own legends, so a session
    /// with the hints off is a session with them off in the file question a
    /// program raises too — see [`button_hints`].
    button_hints: Option<bool>,
    /// Which column of the start screen a session opens on, by the name the
    /// column goes under on the bar rather than the one it is drawn with.
    ///
    /// Not checked against the columns this machine has, on the way in or on
    /// the way out. A name this shell has never heard of goes on being written
    /// back, for the reason [`Stored::steam_sort`] does: a session that read a
    /// later version's setting, changed the accent and wrote the file back
    /// would otherwise throw the user's choice away. And a column that is real
    /// but not here — a Steam library nobody is signed in to — is not a mistake
    /// in the file at all; the shell opens where it always did and the setting
    /// waits. See [`STARTUP_CATEGORY`].
    startup_category: Option<String>,
    /// Whether the file explorer lists the names that begin with a dot.
    /// Session-wide beside the orders in `media_sort`, because it is the same
    /// kind of preference about how a folder is read and it is held the same
    /// way: one answer for every folder, not one per directory.
    show_hidden: Option<bool>,
    /// Whether the corner writes the battery's charge out in figures beside
    /// the level it draws. Session-wide, and written on every machine — a
    /// desktop has no row for it and no mark to apply it to, and neither is a
    /// reason to forget what the laptop this file came from was set to.
    battery_percent: Option<bool>,
    /// How large every application draws its own interface, in per cent of the
    /// size it chose. 100 is one to one and is the least it can be.
    ///
    /// Session-wide, and not in a display's section although it is about what
    /// is on the screens: the two screens on a desk are looked at by the same
    /// pair of eyes from the same chair, and a scale set per display would be a
    /// window that changed size on being moved between them.
    ///
    /// Written by this shell and read by the next one — the compositor
    /// remembers nothing about it, because every application is started after
    /// the shell has connected and said what it is.
    application_scale: Option<u16>,
    /// What a browser's picture-in-picture window is given: whether it floats
    /// over everything at all, how large it is drawn — `small`, `medium` or
    /// `large` — and which of the four corners it sits in.
    ///
    /// Session-wide, beside the application scale and for the same reason: it
    /// is a statement about what such a window *is*, not about one screen.
    /// Written by this shell and read by the next one; the compositor remembers
    /// nothing about it, because the shell says what it is as soon as it
    /// connects and long before any application exists to put a video in.
    ///
    /// A size or a corner this shell has no name for is ignored with a word
    /// about it, and that half keeps the value it had — the same answer a
    /// mistyped display mode gets.
    picture_in_picture: Option<bool>,
    picture_in_picture_size: Option<String>,
    picture_in_picture_place: Option<String>,
    /// Whether the controller is the control the user last reached for, or a
    /// keyboard is. Nothing chooses it; the shell watches for it. See
    /// [`CONTROLLER_IN_HAND`], and note that `false` is the one that does
    /// something — it is what stops a keyboard being offered to somebody
    /// already sitting at one.
    controller_in_hand: Option<bool>,
    /// What every pointing device on this machine does: libinput's acceleration
    /// in hundredths, the cursor's size in logical pixels, how far a wheel
    /// carries the content in per cent, and whether that content follows the
    /// fingers. Settings > Input > Mouse.
    ///
    /// Session-wide, beside the application scale and for its reason: what is
    /// being said is how a hand moves and how well somebody sees, which is true
    /// of the person and not of a screen.
    ///
    /// Written by this shell and read by the next one. The compositor's own
    /// config file has three of these keys and is what a session with no shell
    /// uses; this file wins where there is one, because the shell says what
    /// they are as soon as it connects. A number outside the range the page can
    /// ask for is brought to the nearest end rather than refused, as a mistyped
    /// sound level is: the file is one the user is entitled to open.
    pointer_speed: Option<i8>,
    cursor_size: Option<u16>,
    scroll_speed: Option<u16>,
    natural_scroll: Option<bool>,
    /// Which display the on-screen keyboard comes up on, by connector name.
    /// Absent — which is what every file written before this setting existed
    /// says — is the board following whichever screen is being driven.
    ///
    /// Session-wide, and deliberately *not* in that display's own section
    /// although its value is a connector: what is being said is where the one
    /// board in the session belongs, not something about a screen. A key in
    /// `[display.DP-1]` would be the same fact filed under one of the two
    /// screens it is a choice between.
    ///
    /// A name this session has no display for is read, kept and written back
    /// unchanged, for the reason [`Stored::startup_category`] is: a monitor
    /// that is not plugged in this morning is not a setting the user has
    /// withdrawn, and a shell that quietly rewrote it would lose the answer the
    /// first time anything else on this page was changed.
    keyboard_display: Option<String>,
    /// Which arrangement every keyboard on this machine is set to, as
    /// `layout (variant)` — the form `setxkbmap -query` prints.
    ///
    /// Absent — which is what every file written before this setting existed
    /// says — leaves the compositor's own `keyboard_layout` in force. It is not
    /// the same as `"us"`: a machine whose owner set their layout in the
    /// compositor's config file has answered this question already, and a shell
    /// that wrote `us` here because nobody had opened the page would take that
    /// answer away.
    ///
    /// A layout this machine's xkeyboard-config does not describe is read, kept
    /// and written back unchanged, for the reason [`Stored::keyboard_display`]
    /// is: the registry is a file that changes with a package, and a layout
    /// that went away when one was removed is not a setting the user withdrew.
    keyboard_layout: Option<String>,
    /// What order the Steam column is listed in. One key rather than a table
    /// like `media-sort`, because there is one library.
    ///
    /// Above the two maps, and it has to be: TOML puts everything after a table
    /// header inside that table, so a bare key declared below them would be
    /// written into `[media-sort]` and read back as a shelf.
    steam_sort: Option<String>,
    /// The three switches under Settings > Games > Steam: whether this shell
    /// drives Valve's client at all, whether it starts one as the session comes
    /// up, and whether it leaves one running once a game has ended.
    ///
    /// Written on every machine, including one with no Steam installed. What
    /// they answer is what *this shell* does, and a file carried to a machine
    /// where the client has since been installed should not have lost the
    /// answers on the way. Above the two maps for [`Stored::steam_sort`]'s
    /// reason.
    steam_integration: Option<bool>,
    steam_at_startup: Option<bool>,
    steam_after_a_game: Option<bool>,
    /// Where this machine is, for the night light's sunset-to-sunrise
    /// schedule. Both or neither; degrees, north and east positive.
    ///
    /// Session-wide rather than per display, because a location is: the two
    /// screens on a desk are in the same place. Nothing in the shell writes
    /// these — there is no page for them, because a page asking for a latitude
    /// would be asking the user to go and look one up — and the ordinary answer
    /// comes from the time zone's own coordinates. They are here for the one
    /// case that cannot: somebody a long way from the middle of a large zone.
    /// See [`crate::sun`].
    night_light_latitude: Option<f64>,
    night_light_longitude: Option<f64>,
    /// Which controller the machine's Bluetooth is, by address, and what
    /// happens to it when a session starts.
    ///
    /// The whole of what this shell writes down about Bluetooth. Everything
    /// else — what is paired, what it is called, whether it can be found — is
    /// BlueZ's, and a second copy here would be a second opinion about it at
    /// every login. These three are the ones BlueZ has no answer to: which of
    /// two radios the user means, what to do at startup, and what "as it was
    /// left" refers to.
    bluetooth_controller: Option<String>,
    bluetooth_startup: Option<String>,
    bluetooth_was_on: Option<bool>,
    /// One section per display, by connector name. Sorted, so the file does
    /// not reshuffle itself every time it is written.
    display: BTreeMap<String, StoredDisplay>,
    /// What order each shelf of the user's own files is listed in, by the name
    /// of the row it hangs on. Sorted for the same reason, and a map rather
    /// than three keys because the shelves are a table in [`crate::apps`] and a
    /// fourth one should not need a field here.
    media_sort: BTreeMap<String, String>,
}

/// One display's section of the file.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "kebab-case")]
struct StoredDisplay {
    /// `WIDTHxHEIGHT@REFRESH`, spelled and named as the compositor's own
    /// config spells and names a mode — one format across the two halves of
    /// the session, so what is written here can be pasted there and mean the
    /// same thing. It holds both halves of the page, because a connector is
    /// set to a mode rather than to a size and a rate separately.
    ///
    /// Absent for a display nobody has chosen either half for, which is not
    /// the same as a display set to its preferred mode.
    mode: Option<String>,
    /// `normal`, `90`, `180`, `270`, or one of the four mirrored spellings —
    /// again the compositor's own config's word for the same thing, so a line
    /// can be moved between the two files.
    ///
    /// Absent for a display nobody has turned, which is left the way the
    /// compositor brought it up rather than being asked for `normal`.
    transform: Option<String>,
    /// Which place this screen takes in the row the displays are laid out in,
    /// counted **from one**: `1` is the first screen, which on the ordinary
    /// left-to-right layout is the leftmost.
    ///
    /// From one here and from zero everywhere else in the shell, because this
    /// is the one of the two a person reads: the page calls it Display 1, and a
    /// file that called the same screen `order = 0` would be a file that
    /// disagrees with the page about the user's own monitor.
    ///
    /// Written for every screen at once or for none, unlike every other key
    /// here: an order is a statement about all of them together, and two
    /// screens claiming one place is not an arrangement. A display the file
    /// still names but that is no longer plugged in keeps its place, which is
    /// what brings it back to that place when it returns.
    order: Option<u32>,
    hdr: Option<bool>,
    hdr_sdr_brightness: Option<u16>,
    hdr_srgb_intensity: Option<u8>,
    hdr_peak_brightness: Option<u16>,
    /// Warm this display's picture, and how far — in kelvin, lower being
    /// warmer.
    ///
    /// No top-level default stands behind these, unlike the four HDR keys. A
    /// display nobody has warmed comes up unwarmed, which is what the setting's
    /// own default is and is never the wrong answer for a screen this file has
    /// never heard of.
    night_light: Option<bool>,
    night_light_temperature: Option<u16>,
    /// Which hours it keeps: `all-day`, `sunset-to-sunrise`, or `hours`.
    ///
    /// Separate from the two hours rather than folded into their absence, so a
    /// window survives being set aside: a user who follows the sun for a week
    /// gets their own evening back, not one this shell made up.
    night_light_schedule: Option<String>,
    /// The hours of local time it comes on and goes off at, 0 to 23. Equal
    /// hours are neither a whole day nor none of one, and are dropped.
    night_light_from: Option<u8>,
    night_light_until: Option<u8>,
    /// Rest this display behind black while another one is being used.
    ///
    /// No top-level default stands behind it, as none stands behind the night
    /// light's keys: a display this file has never heard of is never rested,
    /// which is the setting's own default and is what a screen plugged in for
    /// the first time should do.
    oled_protection: Option<bool>,
}

impl Mode {
    /// `1920x1080@60`, or `1920x1080` for a mode with no rate — which is also
    /// what a size chosen with no rate to carry over is written as, and means
    /// the same thing to the compositor: the fastest of that size.
    fn to_config(self) -> String {
        match hertz(self.refresh) {
            Some(hertz) => format!(
                "{}x{}@{}",
                self.resolution.width,
                self.resolution.height,
                hertz.trim_end_matches(" Hz")
            ),
            None => format!("{}x{}", self.resolution.width, self.resolution.height),
        }
    }

    /// The same, read back. `None` for anything that is not a mode, which is
    /// what a hand-edited file is entitled to contain.
    fn from_config(raw: &str) -> Option<Self> {
        let (size, refresh) = match raw.split_once('@') {
            Some((size, refresh)) => (size, Some(refresh)),
            None => (raw, None),
        };
        let (width, height) = size.split_once('x')?;
        let refresh = match refresh {
            // Both `60` and `59.94`, as the compositor's config accepts, since
            // this is meant to be the same format.
            Some(refresh) => {
                let refresh = refresh.trim().trim_end_matches("Hz").trim();
                (refresh.parse::<f64>().ok()? * 1000.0).round().max(0.0) as u32
            }
            None => 0,
        };
        Some(Self {
            resolution: Resolution {
                width: width.trim().parse().ok()?,
                height: height.trim().parse().ok()?,
            },
            refresh,
        })
    }
}

/// Everything the shell is currently set to, in the shape it is written in.
///
/// Built from the live values rather than accumulated as they change, so the
/// file can never record a setting that is not the one in force.
fn stored() -> Stored {
    let inherited = *INHERITED.lock().unwrap();
    let hdr = HDR.lock().unwrap();
    let modes = MODE.lock().unwrap();
    let turns = TURN.lock().unwrap();
    let places = PLACE.lock().unwrap();
    let nights = NIGHT.lock().unwrap();
    let rests = OLED.lock().unwrap();

    // A screen may have been given one of these and not the others, so the
    // sections are the union rather than any one list: writing only the screens
    // with HDR settings would drop a mode the moment it was chosen.
    let mut display: BTreeMap<String, StoredDisplay> = BTreeMap::new();
    for (name, hdr) in hdr.iter() {
        display.entry(name.clone()).or_default().hdr_from(*hdr);
    }
    for (name, mode) in modes.iter() {
        display.entry(name.clone()).or_default().mode = Some(mode.to_config());
    }
    for (name, turn) in turns.iter() {
        display.entry(name.clone()).or_default().transform = Some(turn.key().to_string());
    }
    // Counted from one on the way out, and back to zero on the way in. See
    // [`StoredDisplay::order`] for why this one key disagrees with the rest of
    // the shell about where counting starts.
    for (name, place) in places.iter() {
        display.entry(name.clone()).or_default().order = Some(place + 1);
    }
    for (name, night) in nights.iter() {
        display.entry(name.clone()).or_default().night_from(*night);
    }
    for (name, rest) in rests.iter() {
        display.entry(name.clone()).or_default().oled_protection = Some(*rest);
    }

    let sound = *SOUND.lock().unwrap();
    let playing = *START_MUSIC.lock().unwrap();

    Stored {
        accent: Some(theme::accent().name.to_string()),
        // The applied ones, never a preview: this is written the moment a row is
        // pressed, and a file that recorded what the cursor happened to be
        // standing on would be a setting nobody chose.
        theme_wallpaper: Some(
            theme::applied_style(theme::Part::Wallpaper)
                .name()
                .to_string(),
        ),
        theme_icons: Some(theme::applied_style(theme::Part::Icons).name().to_string()),
        wallpaper_file: custom_wallpaper().map(|file| file.display().to_string()),
        retroarch_roms: roms_folder().map(|at| at.display().to_string()),
        // Never written. See [`Stored::theme`]: this is the key the two above
        // replaced, and writing it as well would be a third opinion about a
        // setting that now has two.
        theme: None,
        sound_volume: Some(sound.value),
        sound_muted: Some(sound.muted),
        start_music: Some(playing),
        do_not_disturb: Some(do_not_disturb()),
        battery_percent: Some(battery_percent()),
        show_hidden: Some(show_hidden()),
        button_hints: Some(button_hints()),
        // The settings themselves and never what this session is *doing*: a
        // session started with `--no-steam` drives no client and has still not
        // been told to stop wanting one, so writing `false` here would be the
        // flag quietly turning the user's setting off. See
        // [`STEAM_IN_THIS_SESSION`], which is why these read the statics rather
        // than the three readers above them.
        steam_integration: Some(*STEAM_INTEGRATION.lock().unwrap()),
        steam_at_startup: Some(*STEAM_AT_STARTUP.lock().unwrap()),
        steam_after_a_game: Some(*STEAM_AFTER_A_GAME.lock().unwrap()),
        startup_category: Some(startup_category()),
        pointer_speed: Some(pointer().speed),
        cursor_size: Some(pointer().size),
        scroll_speed: Some(pointer().scroll),
        natural_scroll: Some(pointer().natural),
        keyboard_display: keyboard_display(),
        // The shell's own answer and never the compositor's. What
        // [`keyboard_layout_in_force`] returns is partly a fact about the
        // machine's other config file, and writing that here would turn a
        // setting the user never made into one they did — after which changing
        // the compositor's file would stop working.
        keyboard_layout: keyboard_layout(),
        controller_in_hand: Some(controller_in_hand()),
        application_scale: Some(app_scale()),
        picture_in_picture: Some(picture_in_picture().floating),
        picture_in_picture_size: Some(picture_in_picture().size.key().to_string()),
        picture_in_picture_place: Some(picture_in_picture().place.key().to_string()),
        hdr: Some(inherited.enabled),
        hdr_sdr_brightness: Some(inherited.sdr_brightness),
        hdr_srgb_intensity: Some(inherited.srgb_intensity),
        hdr_peak_brightness: Some(inherited.peak_brightness),
        bluetooth_controller: bluetooth_controller(),
        bluetooth_startup: Some(bluetooth_startup().key().to_string()),
        bluetooth_was_on: Some(bluetooth_was_on()),
        display,
        media_sort: MEDIA_SORT.lock().unwrap().clone(),
        steam_sort: STEAM_SORT.lock().unwrap().clone(),
        // Written back out so that a file which named a place goes on naming
        // it: everything here is built from the live values, and a key the
        // writer could not see is one the next change to anything else drops.
        night_light_latitude: crate::sun::written_location().map(|at| at.latitude),
        night_light_longitude: crate::sun::written_location().map(|at| at.longitude),
    }
}

impl StoredDisplay {
    fn hdr_from(&mut self, hdr: Hdr) {
        self.hdr = Some(hdr.enabled);
        self.hdr_sdr_brightness = Some(hdr.sdr_brightness);
        self.hdr_srgb_intensity = Some(hdr.srgb_intensity);
        self.hdr_peak_brightness = Some(hdr.peak_brightness);
    }

    /// All five keys, always — the hours included while the light is on all
    /// day, so that a schedule set and then set aside is still there when it
    /// is asked for again.
    fn night_from(&mut self, night: NightLight) {
        self.night_light = Some(night.enabled);
        self.night_light_temperature = Some(night.temperature);
        self.night_light_schedule = Some(night.schedule.key().to_string());
        self.night_light_from = Some(night.from);
        self.night_light_until = Some(night.until);
    }
}

/// Write the settings, through a temporary and a rename — the same care
/// [`crate::pointer::Prefs`] takes, and for the same reason: this is written
/// from a shell running on a machine that gets switched off at the wall, and a
/// half-written file is one the next session cannot read at all.
fn save(stored: &Stored) {
    let Some(path) = settings_path() else {
        return;
    };
    let Some(directory) = path.parent() else {
        return;
    };
    if let Err(err) = std::fs::create_dir_all(directory) {
        tracing::warn!(%err, path = %directory.display(), "could not create the config directory");
        return;
    }

    let body = match toml::to_string_pretty(stored) {
        Ok(body) => format!("{PREAMBLE}{body}"),
        Err(err) => {
            tracing::warn!(%err, "could not serialise the shell settings");
            return;
        }
    };

    let temporary = path.with_extension("toml.new");
    if let Err(err) = std::fs::write(&temporary, body) {
        tracing::warn!(%err, path = %temporary.display(), "could not write the shell settings");
        return;
    }
    if let Err(err) = std::fs::rename(&temporary, &path) {
        tracing::warn!(%err, path = %path.display(), "could not replace the shell settings");
        let _ = std::fs::remove_file(&temporary);
        return;
    }
    published(stored);
}

/// The half of these settings a login screen shows, as it was last told.
///
/// Kept so that it can be told when that half changes and left alone when it
/// does not. Most of what this file holds is none of a login screen's business
/// — how loud the shell's own sounds are, what order a shelf is listed in — and
/// [`set_sound`] deliberately writes the file on every step of a held volume
/// direction, which is a second or two of writes for one press.
type Shown = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<bool>,
    Option<u16>,
    Option<u8>,
    Option<u16>,
    BTreeMap<String, StoredDisplay>,
);

static SHOWN: Mutex<Option<Shown>> = Mutex::new(None);

/// Tell the login screen that these settings have changed.
///
/// A display manager runs as an account of its own and cannot read a home
/// directory, so the only thing it can know about this account is what has been
/// published for it — an avatar, by `accounts-daemon`, and the accent and the
/// display settings by this. Left untold, the copy it reads is the one written
/// when this session started: change the accent to red at lunchtime, sign out
/// in the evening, and the login screen that comes up is still the purple it
/// was that morning.
///
/// Told from here rather than watched for from the other side, because this is
/// the moment it becomes true. Something watching the file has to decide when a
/// rewrite has finished and then race the logout that may follow it — and the
/// one person it would get wrong is somebody who changes a setting and signs
/// straight out, which is exactly the person looking at the login screen next.
///
/// Console Experience Desktop Manager is the display manager that understands
/// this, and it is not required to be installed: on a machine with another
/// login screen none of [`LOGIN_SCREENS`] is found and nothing happens. Nothing
/// is passed to it — it reads the file that has just been written, as the
/// account that wrote it — and nothing it says is waited for, because no
/// decision here rests on the answer.
fn published(stored: &Stored) {
    if !news_for_the_login_screen(stored) {
        return;
    }
    tell_the_login_screen(&LOGIN_SCREENS);
}

/// What that display manager is called, newest name first.
///
/// `cedm` is what it installs as. The long name is what the same program was
/// called before that project shortened its package, its binary, its unit and
/// its configuration directory to the word everybody used for it anyway, and is
/// kept behind it so that a machine still carrying the older greeter is told as
/// well.
///
/// Getting this wrong is silent in both directions, which is why there are two
/// of them: a name nothing on the machine answers to is indistinguishable here
/// from a machine running somebody else's login screen, and the failure it
/// produces is the exact one this whole path exists to prevent — a login screen
/// showing the accent from the *previous* session, because the only copy it
/// ever got was the one `cedm-session` published at sign-in.
///
/// Looked up on `PATH` rather than under a fixed directory: this shell does not
/// know where that package was installed, and a distribution is free to put it
/// somewhere other than `/usr/bin`.
const LOGIN_SCREENS: [&str; 2] = ["cedm", "console-experience-desktop-manager"];

/// Tell the login screen, and wait for it, because this session is ending.
///
/// The ordinary route does not wait — nothing on screen depends on the answer,
/// and the shell has a frame to draw. On the way out there is no frame to draw
/// and waiting is the whole point: a session that exits takes its children with
/// it, so a copy that had not finished being written is a copy that never gets
/// written.
///
/// It is spent here because of the one thing in that copy which cannot be
/// caught any other way. The accent and the displays are written down, so a
/// change to either passes through [`save`] and is published as it happens. How
/// loud the machine is and which device it plays through are the sound server's,
/// not this shell's, and the shell is deliberately not a second opinion about
/// either — so nothing marks the moment they change, and the volume the login
/// screen should answer at is simply whatever it happens to be when the user
/// leaves. This is that moment.
pub fn tell_the_login_screen_before_leaving() {
    /// Long enough for a small program to read two files and write one, and
    /// short enough that nobody watches a screen for it. A login screen that
    /// takes longer than this to answer is one this session will leave behind.
    const AT_MOST: Duration = Duration::from_millis(1500);

    let Some((_, mut child)) = start_the_login_screen(&LOGIN_SCREENS) else {
        return;
    };
    let deadline = Instant::now() + AT_MOST;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            // Left running rather than killed. It writes through a temporary
            // and a rename, so the worst a slow one can do is finish after this
            // shell has gone — which is exactly what was wanted anyway.
            Ok(None) => {
                tracing::debug!("the login screen is still being told; leaving it to finish");
                return;
            }
            Err(err) => {
                tracing::debug!(%err, "could not wait for the login screen");
                return;
            }
        }
    }
}

/// Start the first of `programs` this machine has, and say which it was.
///
/// Every name is tried, because "not found" is what a machine with a different
/// login screen and a machine with an older one both look like from here. Only
/// the first that starts is run: they are names for one program, not several.
fn tell_the_login_screen(programs: &[&str]) -> Option<String> {
    let (program, mut child) = start_the_login_screen(programs)?;
    // Reaped rather than left a zombie: the shell outlives every one of these,
    // and there is one per settings change.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Some(program)
}

/// Start the first of `programs` this machine has, and hand back the process
/// itself along with the name that worked.
///
/// Whether to wait for it is the caller's: an ordinary settings change does not,
/// and a session on its way out does. See
/// [`tell_the_login_screen_before_leaving`].
fn start_the_login_screen(programs: &[&str]) -> Option<(String, std::process::Child)> {
    for program in programs {
        let mut command = std::process::Command::new(program);
        command
            .arg("--publish-look")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match command.spawn() {
            Ok(child) => return Some(((*program).to_string(), child)),
            // Not this name. The next one, and if there is no next one there is
            // nobody to tell, which is not a fault.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => {
                tracing::debug!(
                    %err,
                    program,
                    "could not tell the login screen about the new settings"
                );
                return None;
            }
        }
    }
    None
}

/// Whether the half of these settings a login screen shows has changed since it
/// was last told, and remember the answer.
///
/// A login screen shows an accent and brings displays up; nothing else in this
/// file is any of its business. The distinction matters because the file is
/// written far more often than that half of it changes — [`set_sound`] writes on
/// every step of a held volume direction, deliberately, and one press of that is
/// a second or two of writes. Telling the login screen each time would be a
/// process started for each one.
///
/// The first save of a session is always news, whatever it says. Nothing has
/// been told yet, and a session whose sign-in could not publish gets a second
/// chance the first time anybody changes anything.
fn news_for_the_login_screen(stored: &Stored) -> bool {
    let shown: Shown = (
        stored.accent.clone(),
        stored.theme_wallpaper.clone(),
        stored.theme_icons.clone(),
        stored.hdr,
        stored.hdr_sdr_brightness,
        stored.hdr_srgb_intensity,
        stored.hdr_peak_brightness,
        stored.display.clone(),
    );
    let mut last = SHOWN.lock().unwrap();
    if last.as_ref() == Some(&shown) {
        return false;
    }
    *last = Some(shown);
    true
}

/// What the file says about itself, since it is written by the shell but sits
/// somewhere the user is entitled to open it.
const PREAMBLE: &str = "\
# LineXinBar's own settings, written by the shell.
#
# Editing this by hand is fine; the shell reads it once at startup and
# rewrites it whenever a setting changes from the Settings column.
#
# accent: the colour of being chosen. One of the names the shell offers under
# Settings > Appearance > Accent color. An unknown name is ignored.
#
# theme-wallpaper and theme-icons: how much material each half of the shell is
# drawn with — Default for its own look, or Simple for the plainer one a slow
# machine asks for. The wallpaper is the band of water against the fine ribbons
# the shell drew before it; the icons are marks beaded out of their own shape
# against the flat shapes themselves. Two keys because they cost their own money
# and either may be either way round. Settings > Appearance > Theme. An unknown
# name is read as Default. The login screen reads both keys, and the compositor
# reads the wallpaper's for the frame it opens the session with.
#
# wallpaper-file: the picture or film standing behind everything, where
# theme-wallpaper says Custom wallpaper. It is the shell's own copy of what was
# chosen, under $XDG_DATA_HOME/linexinbar, so that moving or deleting the
# original does not take the wallpaper with it — choose the file again from
# Settings > Appearance > Theme > Wallpaper > Custom wallpaper to replace it. A
# film is drawn without its sound, which is not a setting: nothing in this shell
# decodes audio. A file that is not there when the session starts is not an
# error; the shell draws its own wallpaper and leaves this key alone, because
# the drive it is on may be plugged in later. The login screen and the
# compositor both ignore this and draw the shell's own scene — neither of them
# can read a file under your home.
#
# theme: what those two were before they were two settings. Read where a half
# says nothing of its own, never written, and replaced by the pair at the next
# save — a machine deliberately set to Simple stays there across the change.
#
# sound-volume: how loud the shell's effects and Start music are, 0 to 1, and
# sound-muted whether they are silenced. Both are the System row of the volume
# mixer, in the guide overlay. These are the shell's own sounds and nothing
# else's; what the whole machine comes out at belongs to the sound server, and
# the volume bar in the same overlay sets it there.
#
# steam-sort: what order the Steam column is listed in, chosen from the Sort
# row of the context menu over any game in it. One key, because there is one
# library. The orders are: installed-first, name, name-reversed, last-played,
# play-time-most-first, play-time-least-first, size-largest-first,
# size-smallest-first. Without it the column is listed installed first, each
# half by name. An order Steam cannot answer for — sizes on a machine with
# nothing installed, playtimes an account did not deliver — is greyed out in
# the menu and ignored here.
#
# steam-integration: whether this shell drives Valve's client at all, which is
# Settings > Games > Steam > Integration. On unless this says false, including
# for a file written before the key existed. Off, there is no Steam row at the
# head of Games and no library column: Steam is an application like any other,
# its own .desktop entry stands wherever the scan filed it wearing the icon its
# package ships, and nothing in this session signs an account in, hides one of
# the client's windows or starts a game for it. A session started with
# --no-steam does none of that either, and this key is left alone by it: the
# flag is what that session was told to do, and the setting is what the machine
# is set to.
#
# steam-at-startup: whether Valve's client is started in the background as the
# session comes up rather than when the first game is pressed — Settings >
# Games > Steam > Start with the shell. Off unless this says true. On, the
# client's cold start is paid once while nobody is looking, at the cost of its
# memory for the whole session on a machine where nobody plays anything.
#
# steam-after-a-game: whether Valve's client is left running once a game has
# ended — Settings > Games > Steam > Leave Steam running. It is left running
# unless this says false, which is what this shell has always done. False, the
# client is asked to shut down five seconds after the last Steam game's window
# has gone — but not while it is fetching, verifying or removing anything, not
# while another game is starting, not while a window of Steam's own is being
# looked at, and never when it is a client this session did not start. Those
# hold the shutdown off for as long as they last rather than for the session:
# quitting a game is what lets the client pick up the update it has been
# holding, and dropping the moment there left a client running all evening with
# this set to false. And equally when a press started the client and no game
# ever came of it: Start Steam on a game with an update waiting, or a loading
# screen left to its download. Those end with the client doing the work and
# then sitting there, so it is closed when that work is done — including the
# first stretch of an update, where the manifest says nothing at all about what
# Steam is doing. A client that declines to shut down is asked once more and
# then left alone, with a line in the log saying so — and one belonging to
# another session on this machine is not asked at all, which the log says
# instead of pretending it refused.
#
# start-music: whether the Start screen plays its background music, which is
# Settings > Sounds > Start music. It plays unless this says false. Turning it
# off leaves every other sound the shell makes exactly as loud as it was; how
# loud that is, the music included, is the two keys above.
#
# battery-percent: whether the start screen's corner writes the battery's
# charge out in figures beside the mark that draws it, which is Settings >
# Appearance > Battery percentage. Off unless this says true. Both the row and
# the mark itself exist only on a machine that has a battery — a desktop shows
# neither, and this key is kept for it anyway so that a file carried between
# the two does not lose the setting on the way.
#
# button-hints: whether the shell writes what its buttons do — a picture of
# each button and the word for what it does — which is Settings > System >
# Button hints. On unless this says false, and on for a file written before the
# key existed: a console is the one kind of machine nobody arrives at already
# knowing which button does what, so the legend has to be there before anybody
# thinks to look for a setting that would reveal it.
#
# One answer for every screen that has one: the corner of the start screen
# opposite its clock, the foot of the menu the Home button opens, the friends
# list, the chip that says which two buttons summon the on-screen keyboard, and
# the foot of the file question an application asks. Applications built on
# lxb-toolkit read this key too and write their own legends from it, which is
# why it is here rather than kept to the shell.
#
# startup-category: which column of the start screen a session opens on, by the
# name that column goes under on the bar — settings, system, software,
# multimedia, graphics, internet, office, games, steam, retroarch, development,
# education, utilities, waydroid, other. Games unless this says otherwise,
# including for a file written before the key existed. A name naming a column
# this machine has not got is not an error and is not rewritten: the shell opens
# on the first column with anything in it, as it always did, and the setting
# waits for the account to be signed in or the package to be installed again.
# Settings > System > Startup category.
#
# do-not-disturb: whether anything may interrupt, which is the moon tile at the
# head of the guide overlay's column rather than a row of the Settings column.
# On, an announcement is filed without a bubble in the corner and without a
# chime; nothing is discarded, and the tile beside that one lists what arrived.
# Written down because it is a switch somebody threw on purpose: a console that
# had quietly turned it off overnight would deliver a night of announcements at
# breakfast.
#
# controller-in-hand: which control the shell last saw the user reach for. It
# is not chosen anywhere — the shell watches for it, on a button or a stick on
# the controller and on any key on a keyboard — and it decides one thing: what
# is offered to a controller. False, the chip in the corner naming the buttons
# that summon the on-screen keyboard stays away, and no text field brings that
# keyboard up by itself; a keyboard drawn over the shoulder of somebody typing
# is a picture of the keys already under their hands. Any button on the pad
# brings both back. Set it by hand if you like; the next thing you touch has
# the last word.
#
# keyboard-display: which screen the on-screen keyboard comes up on, by the
# connector's name — the ones lxb logs at startup, and the ones the [display]
# sections below are filed under. Settings > Input > Keyboard > On-screen
# keyboard > Default display. Without it the board follows whichever screen is being
# driven, which is where it belongs on a machine with one screen and on most
# with two: the board is summoned by a hand, and the hand is at the screen
# being looked at. Name one for the desk where it is not — a handheld panel
# beside a television, a touchscreen beside a monitor — and the board comes up
# there whatever is being driven.
#
# A name this session has no display for is not an error and is not rewritten.
# The board falls back to the screen being driven for as long as that display
# is unplugged and goes back to it the moment it returns, which is what every
# [display] section does with a screen that comes and goes: a monitor
# unplugged for an afternoon is not somebody changing their mind about where
# their keyboard belongs.
#
# keyboard-layout: which arrangement every keyboard on this machine is set to,
# written the way setxkbmap -query prints it — the xkb layout, and the variant
# in brackets after it where there is one: pl, us (dvorak), fr (azerty).
# Settings > Input > Keyboard > Keyboard layout, where the list is read off
# this machine's own xkeyboard-config and filed by continent and by country.
#
# Absent is not the same as us. It means this shell has no opinion, and what
# the keyboard does is then whatever the compositor's own config.toml says
# under [input] — so a machine whose owner set keyboard_layout there by hand
# keeps that answer until somebody picks a row on this page. Once one is
# picked, this key wins.
#
# The shell's own on-screen keyboard follows it: the caps show what the chosen
# arrangement types, with AltGr for the level most accented letters live on. A
# layout this machine's xkeyboard-config does not describe is not an error and
# is not rewritten — xkeyboard-config is a package, and a layout that went away
# when one was removed is not somebody changing their mind about their
# keyboard.
#
# pointer-speed, cursor-size, scroll-speed and natural-scroll: what every
# pointing device on this machine does, which is Settings > Input > Mouse. All
# four reach a mouse, a touchpad, a trackball and a controller's trackpad alike:
# they all move the same pointer, and a setting that applied to one of them
# would be a page that works on some desks and not others.
#
# pointer-speed is libinput's own acceleration in hundredths, -100 to 100, and 0
# — the middle of its range, not the slow end — is the flat default every
# desktop starts at. It is passed through rather than translated because what
# libinput does with it depends on the device's own resolution; the page shows
# where the handle stands on its track, in per cent, which is a different
# number for the same thing.
#
# cursor-size is the pointer's size in logical pixels, as XCURSOR_SIZE counts
# it: 16, 24, 32 or 48. Anything else is read as the nearest of those, because
# a cursor theme carries a handful of drawn sizes and the nearest is what gets
# used. It changes the pointer the compositor draws, and the size handed to
# every application started afterwards; it cannot reach one already running,
# which reads its own theme when it starts.
#
# scroll-speed is how far one turn of a wheel carries the content, in per cent
# of what the device reported. 100 is one to one. libinput has no scroll speed,
# so this is the compositor multiplying the movements it forwards — applied to
# the notch count as well as to the distance, so that a program counting wheel
# clicks and one reading pixels agree about how far one click went.
#
# natural-scroll true is content that follows the fingers, which is the
# direction a touchscreen moves; false is the wheel's traditional direction, and
# is what a session that has never been asked does.
#
# The compositor's own config file has three of these keys, under [input], and
# they are what a session running without this shell uses. This file wins where
# there is one: the shell says what these are as soon as it connects.
#
# application-scale: how large every application draws its own interface, in
# per cent of the size it chose, which is Settings > System > Application
# scaling. 100 is one to one and is the least it can be; a smaller number is
# read as 100, and anything past 300 as 300. It is one number for the whole
# session rather than one per display, because what it answers is how far from
# the screens the user is sitting.
#
# It is carried out by the compositor, which gives each window a logical size
# this much smaller than the display and tells it to fill that with the
# display's own pixels — so an interface is drawn larger without losing any
# sharpness, exactly as it is on a high-density laptop panel. The shell's own
# picture is not affected, and neither are windows running under Xwayland:
# X11 has no per-surface scale to be told about, so the only thing that could
# be done to those is to magnify pixels they have already drawn.
#
# picture-in-picture, picture-in-picture-size and picture-in-picture-place:
# what happens to the small window a browser puts a video into, which is
# Settings > System > Picture-in-Picture. It floats over everything unless the
# first says false — over applications, over a fullscreen game, over the start
# screen and over the guide — at small, medium or large, which is a sixth, a
# quarter or a third of the display's width, in the corner named by top-left,
# top-right, bottom-left or bottom-right. Without them it floats, medium, top
# right.
#
# How tall the window is at that width is the window's own business: the
# compositor asks it what shape it wants to be and follows the answer, so a
# four-to-three video is not letterboxed into a widescreen box. A second such
# window, opened while the first is still up, stands below it in a column from
# the same corner.
#
# The window is found by its title, which is Picture-in-Picture and is what
# every browser that has the feature calls it. Turned off, such a window is an
# application window like any other: it fills the screen, it is listed in the
# guide, and it takes the keyboard. A size or a corner spelled some other way
# is ignored with a line in the log, and that half keeps the answer it had.
#
# It is carried out by the compositor, which is what places windows; this file
# is only where the answer is remembered between sessions.
#
# Everything under [display.NAME] is Settings > Display for the connector of
# that name, and is carried out by the compositor rather than by the shell.
# Connector names are the ones lxb logs at startup.
#
# The four hdr keys at the top level, outside any [display] section, are what
# a display with no section of its own is set to — including one plugged in
# for the first time. The mode has no such default: it names something one
# connector offers, and the display beside it may not offer it at all.
#
# mode:                 WIDTHxHEIGHT@REFRESH, as the compositor's own config
#                       spells a mode: 2560x1440@144, or 1920x1080 for the
#                       fastest mode of that size. Both halves of it, because
#                       a connector is set to a mode — Settings > Display
#                       asks for the resolution and the refresh rate
#                       separately and they meet here. Omit it to leave the
#                       display at whatever the compositor brought it up at.
#                       A size the display does not offer is refused, and the
#                       display keeps the mode it has.
# transform:            which way up the picture is drawn, again as the
#                       compositor's own config spells it: normal, 90, 180,
#                       270, or flipped, flipped-90, flipped-180,
#                       flipped-270 for a mirrored picture. Settings >
#                       Display > Orientation offers the four turns; the
#                       mirrored four can be set here and are named there.
#                       Omit it to leave the display the way the compositor
#                       brought it up.
# order:                which place this screen takes in the row the displays
#                       are laid out in, counted from one: 1 is the first
#                       screen, which on the ordinary left-to-right layout is
#                       the leftmost. Settings > Display > Display order sets
#                       it, and writes one for every screen at once — an order
#                       is a statement about all of them together, and two
#                       screens claiming one place is not an arrangement. A
#                       screen this file still names but that is unplugged
#                       keeps its place and comes back to it. Omit them all to
#                       leave the displays in the order they were plugged in,
#                       which is what the compositor does by itself.
# hdr:                  drive this display in high dynamic range.
# hdr-sdr-brightness:   what plain white is sent at, in cd/m².
# hdr-srgb-intensity:   how far sRGB colour is stretched towards BT.2020,
#                       0 for the colour SDR showed, 100 for vivid. Needs a
#                       driver with a degamma stage; where there is none the
#                       compositor says so and this has no effect.
# hdr-peak-brightness:  the peak declared to the display, in cd/m².
#                       0 means whatever the display says about itself.
#
# The five night-light keys are Settings > Display > Night light, the blue
# light filter. They have no top-level default: a display this file has never
# heard of comes up unwarmed, which is what the setting does when nobody has
# asked for it.
#
# night-light:            warm this display's picture. Off is off whatever the
#                         schedule below says.
# night-light-temperature how warm, in kelvin — lower is warmer. 6500 is
#                         daylight and no filter at all, 4000 an ordinary
#                         evening, 2000 candlelight. Anything outside
#                         2000..6500 is brought to the nearest end.
# night-light-schedule:   when it burns. One of: all-day, for as long as
#                         night-light is on; sunset-to-sunrise, which follows
#                         the sun where this machine is; or hours, which keeps
#                         the two below.
# night-light-from:       the hour of local time it comes on at, 0 to 23.
# night-light-until:      the hour it goes off at, exclusive, wrapping past
#                         midnight — 22 and 6, which is what the switch starts
#                         on, is an evening. The two may not be the same hour;
#                         a file that says they are keeps no hours at all.
#
# oled-protection is Settings > Display > OLED protection: fade this screen to
# black once it has been left alone for five seconds, while another one is
# being used. Off unless this says true, and it has no top-level
# default for the reason the night light's keys have none — a display this file
# has never heard of is never rested.
#
# What it is for is a panel that keeps the picture it is shown. The start
# screen is the worst case there is: the bar sits in the same row of pixels
# every second it is up, and a second screen left on it through an evening's
# play is a screen with a bar burnt into it. The black is the compositor's own
# sheet over the whole display, the cursor and anything running on it included,
# and it never takes any input away — moving the pointer onto the screen brings
# it back in a quarter of a second, as does taking that display over or
# pressing anything on it.
#
# Three things stop a screen being rested, and none of them can be set here:
# the screen being driven is never rested, nor is one with something still
# painting on it — a film playing on the second screen is exactly what a second
# screen is for — and nothing is rested at all unless something is open
# somewhere, whatever it is: a game, a film, a browser or an emulator all count
# equally. A film somebody paused is deliberately not spared: a paused film is
# a still picture, which is the thing this exists for.
#
# night-light-latitude and night-light-longitude, at the top level, are where
# this machine is, in degrees — north and east positive. They are what
# sunset-to-sunrise is worked out from, and they are only needed when the
# ordinary answer is not good enough: without them the coordinates come from
# the time zone's own entry in the system's zone table, which is the city the
# zone is named for. Set them for somewhere a long way from that city. Both or
# neither; a pair that is not a place on the earth is ignored.
#
# [media-sort] is what order the rows of the user's own files are listed in,
# one key per shelf — Music, Video, Images — plus Files, which is every folder
# of the file explorer under System. All of them are chosen from the Sort row of
# the context menu over any file in them. The orders are: name, name-reversed,
# size-largest-first, size-smallest-first, type, created-newest-first,
# created-oldest-first, modified-newest-first, modified-oldest-first. A shelf
# with no key here is listed by name.

";

/// `$XDG_CONFIG_HOME/lxb/shell.toml`, beside the compositor's own config
/// and the per-application settings.
fn settings_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from)?;
            Some(home.join(".config"))
        })?;
    Some(config.join("lxb").join("shell.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::system::Device;

    /// The Settings column, built against a bar with every column the shell can
    /// have on it.
    ///
    /// Shadows [`super::column`], which takes the bar it is being built into —
    /// see [`startup_category_page`], the one row that needs it. Every test here
    /// is about a page rather than about which columns this machine happens to
    /// have, so they are all handed the whole set: it is fixed, it does not read
    /// the filesystem, and it means a test can name any column and find it.
    fn column() -> Vec<Entry> {
        super::column(&crate::apps::every_column())
    }

    /// Made-up connector names, and a made-up peak.
    ///
    /// Deliberately not the names of anybody's hardware. Nothing in this
    /// module may depend on what is plugged into the machine it is built on —
    /// the screen list comes from the compositor and the settings are filed
    /// under whatever it calls them — and a suite written around one
    /// developer's monitors is exactly how that dependency gets in without
    /// anyone noticing. These are strings the code has never seen before, and
    /// the deliberately awkward third one is here because a connector name is
    /// not required to be a tidy identifier.
    const FIRST: &str = "TEST-OUT-1";
    const SECOND: &str = "TEST-OUT-2";
    const AWKWARD: &str = "Test Out.3 (left)";
    const PEAK: u16 = 600;

    /// A display that reports everything working.
    fn capable(peak: u16) -> Support {
        Support {
            available: true,
            active: false,
            peak,
            gamut: true,
            night_light: true,
            warming: false,
        }
    }

    /// A display that has a colour ramp and nothing else — an ordinary SDR
    /// panel, which is what most screens are and what the Night light page has
    /// to work on while the HDR page beside it lists nothing.
    fn warmable() -> Support {
        Support {
            night_light: true,
            ..Support::default()
        }
    }

    /// The four controls for one screen, however the page reaches them.
    ///
    /// The screen level is skipped when only one screen can do HDR, so a test
    /// that wants the controls has to be able to find them either way — which
    /// is also the clearest statement of what that collapse means: the same
    /// page, one step nearer.
    fn controls_for(name: &str) -> Vec<Entry> {
        let page = hdr_page();
        let capable = support()
            .into_iter()
            .filter(|(_, support)| support.available)
            .count();
        if capable == 1 {
            return page;
        }
        page.iter()
            .find(|entry| entry.title() == name)
            .unwrap_or_else(|| panic!("{name} is not in the screen list"))
            .entries()
            .expect("a screen opens its settings")
            .to_vec()
    }

    /// What the whole module is set to, so a test can put it back.
    ///
    /// The state is global, as the theme's is, and tests run in parallel — so
    /// everything below serialises on [`LOCK`] the way [`theme::with_accent`]
    /// does. Without it a test that left the settings changed would be a test
    /// that decides what the next one starts from.
    static LOCK: Mutex<()> = Mutex::new(());

    struct Saved {
        hdr: BTreeMap<String, Hdr>,
        inherited: Hdr,
        support: Vec<(String, Support)>,
        offered: Vec<(String, Vec<Offered>)>,
        mode: BTreeMap<String, Mode>,
        reported_turns: Vec<(String, Orientation)>,
        turn: BTreeMap<String, Orientation>,
        reported_places: Vec<(String, u32)>,
        place: BTreeMap<String, u32>,
        night: BTreeMap<String, NightLight>,
        oled: BTreeMap<String, bool>,
        screen_rest: bool,
        sound: Level,
        app_scale: u16,
        pip: Pip,
        start_music: bool,
        do_not_disturb: bool,
        battery_percent: bool,
        show_hidden: bool,
        button_hints: bool,
        steam_integration: bool,
        steam_at_startup: bool,
        steam_after_a_game: bool,
        steam_in_this_session: bool,
        startup_category: Option<String>,
        keyboard_display: Option<String>,
        keyboard_layout: Option<String>,
        keyboard_layout_available: bool,
        compositor_layout: Option<String>,
        layout_query: String,
        pointer: Pointer,
        battery: Option<crate::power::Charge>,
        controller_in_hand: bool,
        devices: Devices,
        network: crate::network::Listing,
        bluetooth: crate::bluetooth::Listing,
    }

    fn take_settings() -> Saved {
        let saved = Saved {
            hdr: HDR.lock().unwrap().clone(),
            inherited: *INHERITED.lock().unwrap(),
            support: support(),
            offered: modes(),
            mode: MODE.lock().unwrap().clone(),
            reported_turns: turned(),
            turn: TURN.lock().unwrap().clone(),
            reported_places: placed(),
            place: PLACE.lock().unwrap().clone(),
            night: NIGHT.lock().unwrap().clone(),
            oled: OLED.lock().unwrap().clone(),
            screen_rest: screen_rest_available(),
            sound: *SOUND.lock().unwrap(),
            app_scale: app_scale(),
            pip: picture_in_picture(),
            start_music: start_music(),
            do_not_disturb: do_not_disturb(),
            battery_percent: battery_percent(),
            show_hidden: show_hidden(),
            button_hints: button_hints(),
            // The statics rather than the readers, because the readers fold the
            // flag in and this has to be able to put back exactly what it took.
            steam_integration: *STEAM_INTEGRATION.lock().unwrap(),
            steam_at_startup: *STEAM_AT_STARTUP.lock().unwrap(),
            steam_after_a_game: *STEAM_AFTER_A_GAME.lock().unwrap(),
            steam_in_this_session: *STEAM_IN_THIS_SESSION.lock().unwrap(),
            startup_category: STARTUP_CATEGORY.lock().unwrap().clone(),
            keyboard_display: keyboard_display(),
            keyboard_layout: keyboard_layout(),
            keyboard_layout_available: keyboard_layout_available(),
            compositor_layout: COMPOSITOR_LAYOUT.lock().unwrap().clone(),
            layout_query: layout_query(),
            pointer: pointer(),
            battery: *BATTERY.lock().unwrap(),
            controller_in_hand: controller_in_hand(),
            devices: DEVICES.lock().unwrap().clone(),
            network: network_listing(),
            bluetooth: bluetooth_listing(),
        };
        HDR.lock().unwrap().clear();
        MODE.lock().unwrap().clear();
        TURN.lock().unwrap().clear();
        PLACE.lock().unwrap().clear();
        NIGHT.lock().unwrap().clear();
        OLED.lock().unwrap().clear();
        // A compositor that can rest a screen, which is what the session ships
        // with. The page that says otherwise is tested by asking for it — see
        // [`a_session_that_cannot_rest_a_screen_says_so`].
        note_screen_rest(true);
        *APP_SCALE.lock().unwrap() = NATURAL_SCALE;
        *PICTURE_IN_PICTURE.lock().unwrap() = Pip::DEFAULT;
        note_turned(Vec::new());
        note_places(Vec::new());
        note_devices(Devices::none());
        note_network(crate::network::Listing::none());
        note_bluetooth(crate::bluetooth::Listing::none());
        note_battery(None);
        note_startup_category(None);
        // A session that does Steam, which is what the shell ships as. The page
        // a `--no-steam` session gets is tested by asking for it — see
        // [`a_session_told_to_leave_steam_alone_says_so`].
        note_steam_in_this_session(true);
        *STEAM_INTEGRATION.lock().unwrap() = true;
        *STEAM_AT_STARTUP.lock().unwrap() = false;
        *STEAM_AFTER_A_GAME.lock().unwrap() = true;
        *KEYBOARD_DISPLAY.lock().unwrap() = None;
        *POINTER.lock().unwrap() = Pointer::DEFAULT;
        *INHERITED.lock().unwrap() = Hdr::default();
        saved
    }

    fn put_back(saved: Saved) {
        *HDR.lock().unwrap() = saved.hdr;
        *INHERITED.lock().unwrap() = saved.inherited;
        *MODE.lock().unwrap() = saved.mode;
        *TURN.lock().unwrap() = saved.turn;
        *PLACE.lock().unwrap() = saved.place;
        *NIGHT.lock().unwrap() = saved.night;
        *OLED.lock().unwrap() = saved.oled;
        note_screen_rest(saved.screen_rest);
        *SOUND.lock().unwrap() = saved.sound;
        *APP_SCALE.lock().unwrap() = saved.app_scale;
        *PICTURE_IN_PICTURE.lock().unwrap() = saved.pip;
        *START_MUSIC.lock().unwrap() = saved.start_music;
        *DO_NOT_DISTURB.lock().unwrap() = saved.do_not_disturb;
        *BATTERY_PERCENT.lock().unwrap() = saved.battery_percent;
        *SHOW_HIDDEN.lock().unwrap() = saved.show_hidden;
        *BUTTON_HINTS.lock().unwrap() = saved.button_hints;
        *STEAM_INTEGRATION.lock().unwrap() = saved.steam_integration;
        *STEAM_AT_STARTUP.lock().unwrap() = saved.steam_at_startup;
        *STEAM_AFTER_A_GAME.lock().unwrap() = saved.steam_after_a_game;
        *STEAM_IN_THIS_SESSION.lock().unwrap() = saved.steam_in_this_session;
        *STARTUP_CATEGORY.lock().unwrap() = saved.startup_category;
        *KEYBOARD_DISPLAY.lock().unwrap() = saved.keyboard_display;
        *KEYBOARD_LAYOUT.lock().unwrap() = saved.keyboard_layout;
        *KEYBOARD_LAYOUT_AVAILABLE.lock().unwrap() = saved.keyboard_layout_available;
        *COMPOSITOR_LAYOUT.lock().unwrap() = saved.compositor_layout;
        *LAYOUT_QUERY.lock().unwrap() = saved.layout_query;
        *POINTER.lock().unwrap() = saved.pointer;
        note_battery(saved.battery);
        *CONTROLLER_IN_HAND.lock().unwrap() = saved.controller_in_hand;
        note_support(saved.support);
        note_modes(saved.offered);
        note_turned(saved.reported_turns);
        *PLACED.lock().unwrap() = saved.reported_places;
        note_devices(saved.devices);
        note_network(saved.network);
        note_bluetooth(saved.bluetooth);
    }

    /// Run `body` with the machine reported as having this battery — or none —
    /// and put back whatever the process had.
    fn with_battery(reported: Option<crate::power::Charge>, body: impl FnOnce()) {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());

        let saved = take_settings();
        note_battery(reported);

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));

        put_back(saved);
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// The Appearance page, however many rows this machine's hardware puts on
    /// it.
    fn appearance_page() -> Vec<Entry> {
        appearance()
            .entries()
            .expect("Appearance opens onto its rows")
            .to_vec()
    }

    /// Run `body` with the network manager reported as saying this, and put
    /// back whatever the process had.
    fn with_network(reported: crate::network::Listing, body: impl FnOnce()) {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());

        let saved = take_settings();
        note_network(reported);

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));

        put_back(saved);
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// Run `body` with BlueZ reported as saying this, and put back whatever the
    /// process had.
    fn with_bluetooth(reported: crate::bluetooth::Listing, body: impl FnOnce()) {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());

        let saved = take_settings();
        note_bluetooth(reported);

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));

        put_back(saved);
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// Run `body` with the sound server reported as offering these devices, and
    /// put back whatever the process had.
    fn with_devices(reported: Devices, body: impl FnOnce()) {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());

        let saved = take_settings();
        note_devices(reported);

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));

        put_back(saved);
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// A device, as a test names one. Invented hardware: nothing here may
    /// depend on what is plugged into the machine this is built on.
    fn device(id: &str, title: &str, profile: Option<&str>, default: bool) -> Device {
        Device {
            id: id.to_string(),
            title: title.to_string(),
            profile: profile.map(str::to_string),
            default,
        }
    }

    /// The rows under Settings > Sounds, owned — reading a title off a borrow
    /// of a freshly built catalogue reads into a temporary.
    fn sounds_page() -> Vec<Entry> {
        column()
            .iter()
            .find(|entry| entry.title() == "Sounds")
            .expect("Settings has a Sounds row")
            .entries()
            .expect("Sounds opens a column")
            .to_vec()
    }

    /// One of the two device rows, and what it opens onto.
    fn device_row(title: &str) -> Entry {
        sounds_page()
            .iter()
            .find(|entry| entry.title() == title)
            .unwrap_or_else(|| panic!("Sounds has no {title} row"))
            .clone()
    }

    /// Run `body` with the display settings empty and `displays` reported, and
    /// put back whatever the process had.
    fn with_displays(displays: &[(&str, Support)], body: impl FnOnce()) {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());

        let saved = take_settings();
        note_support(
            displays
                .iter()
                .map(|(name, support)| (name.to_string(), *support))
                .collect(),
        );

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));

        put_back(saved);
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// One screen and the modes it offers, as `(width, height, hertz)`.
    type Reported<'a> = (&'a str, &'a [(u32, u32, u32)]);

    /// The same for the mode lists: `displays` reported as offering these
    /// modes, and nothing else touched.
    fn with_modes(displays: &[Reported<'_>], body: impl FnOnce()) {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());

        let saved = take_settings();
        note_modes(
            displays
                .iter()
                .map(|(name, modes)| (name.to_string(), listed(modes)))
                .collect(),
        );

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));

        put_back(saved);
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// The same again for the orientations: `displays` reported as being drawn
    /// this way up, and nothing else touched.
    fn with_turns(displays: &[(&str, Orientation)], body: impl FnOnce()) {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());

        let saved = take_settings();
        note_turned(
            displays
                .iter()
                .map(|(name, turn)| (name.to_string(), *turn))
                .collect(),
        );

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));

        put_back(saved);
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// And again for the arrangement: `displays` reported as standing in this
    /// order, first screen first, and nothing else touched.
    ///
    /// Takes the order rather than a place per screen, because that is what an
    /// arrangement is — and because a test that had to hand out its own place
    /// numbers could write down an order with two second screens in it, which
    /// no compositor can report.
    fn with_places(displays: &[&str], body: impl FnOnce()) {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());

        let saved = take_settings();
        note_places(
            displays
                .iter()
                .enumerate()
                .map(|(place, name)| (name.to_string(), place as u32))
                .collect(),
        );

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(body));

        put_back(saved);
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }

    /// The place the shell is asking one display to stand in, if it has ever
    /// been asked for one. Only the tests ask: what the shell sends is the
    /// whole order — see [`wanted_order`] — because a place is not something
    /// one display has on its own.
    fn place_for(display: &str) -> Option<u32> {
        PLACE.lock().unwrap().get(display).copied()
    }

    /// Modes from `(width, height, hertz)`, the first of which is the one the
    /// display is running and the one it says it wants.
    fn listed(modes: &[(u32, u32, u32)]) -> Vec<Offered> {
        modes
            .iter()
            .enumerate()
            .map(|(index, (width, height, hertz))| Offered {
                mode: Mode {
                    resolution: Resolution {
                        width: *width,
                        height: *height,
                    },
                    refresh: hertz * 1000,
                },
                current: index == 0,
                preferred: index == 0,
            })
            .collect()
    }

    /// A whole mode, as a test names one.
    fn mode(width: u32, height: u32, hertz: u32) -> Mode {
        Mode {
            resolution: Resolution { width, height },
            refresh: hertz * 1000,
        }
    }

    /// The Display subcategory's row of that name, owned — reading a comment
    /// off a borrow of a freshly built catalogue reads into a temporary.
    fn display_row(title: &str) -> Entry {
        column()[1]
            .entries()
            .expect("Display opens a column")
            .iter()
            .find(|entry| entry.title() == title)
            .unwrap_or_else(|| panic!("Display has no {title} row"))
            .clone()
    }

    /// The literal HDR badge belongs to controls that configure HDR, not to a
    /// read-only explanation that happens to sit somewhere in Settings.
    #[test]
    fn hdr_badge_is_reserved_for_hdr_rows() {
        let mut without_gamut = capable(PEAK);
        without_gamut.gamut = false;

        with_displays(&[(FIRST, without_gamut)], || {
            assert_eq!(hdr_row().icon(), Some(icons::SETTING_HDR));

            let controls = controls_for(FIRST);
            assert_eq!(controls[0].title(), "HDR");
            assert_eq!(controls[0].icon(), Some(icons::SETTING_HDR));

            let unavailable = controls[2]
                .entries()
                .expect("the unavailable colour control explains why");
            assert_eq!(unavailable[0].icon(), Some(icons::SETTING_INFO));
        });

        for reading in [nothing_reports_modes(), nothing_supports_hdr()] {
            assert_eq!(reading.icon(), Some(icons::SETTING_INFO));
            assert_ne!(reading.icon(), Some(icons::SETTING_HDR));
        }
    }

    /// The page the screen list was added for: Settings, into Display, into
    /// HDR, onto a *screen*, and only then onto the settings.
    ///
    /// The screens are whatever the compositor reported, under whatever it
    /// called them — which is why these names are ones no machine has.
    #[test]
    fn hdr_names_the_screen_before_it_offers_a_setting() {
        with_displays(
            &[
                (FIRST, capable(PEAK)),
                (SECOND, capable(0)),
                (AWKWARD, capable(PEAK)),
            ],
            || {
                let column = column();
                assert_eq!(column[1].title(), "Display");

                let hdr = hdr_row();
                assert_eq!(hdr.title(), "HDR");

                // Every screen that can do it, named as the compositor names
                // it, in the order it announced them.
                let screens = hdr.entries().expect("HDR opens a column");
                assert_eq!(
                    screens.iter().map(Entry::title).collect::<Vec<_>>(),
                    [FIRST, SECOND, AWKWARD]
                );
                // The one that reports a peak says so; the one that does not
                // stays quiet about it rather than claiming zero.
                assert_eq!(
                    screens[0].comment(),
                    Some(format!("Ready, peak {PEAK} cd/m²").as_str())
                );
                assert_eq!(screens[1].comment(), Some("Ready"));

                for screen in screens {
                    let page = screen.entries().expect("a screen opens its settings");
                    assert_eq!(
                        page.iter().map(Entry::title).collect::<Vec<_>>(),
                        [
                            "HDR",
                            "SDR brightness",
                            "sRGB color intensity",
                            "Peak brightness",
                        ]
                    );
                    for control in page {
                        let values = control.entries().expect("a control opens its values");
                        assert_eq!(
                            values.iter().filter(|entry| entry.chosen()).count(),
                            1,
                            "{} on {} has one value in force",
                            control.title(),
                            screen.title()
                        );
                    }
                }
            },
        );
    }

    /// With one screen there is nothing to choose between, so the step that
    /// would ask which one is not there: HDR opens straight onto the settings,
    /// and says in its own comment which screen they belong to.
    #[test]
    fn one_screen_is_not_something_to_choose_between() {
        with_displays(&[(FIRST, capable(PEAK))], || {
            let hdr = hdr_row();
            assert_eq!(hdr.title(), "HDR");
            assert_eq!(
                hdr.comment(),
                Some(format!("{FIRST} — Ready, peak {PEAK} cd/m²").as_str()),
                "the one screen is named where the list would have been"
            );

            let page = hdr.entries().expect("HDR opens a column");
            assert_eq!(
                page.iter().map(Entry::title).collect::<Vec<_>>(),
                [
                    "HDR",
                    "SDR brightness",
                    "sRGB color intensity",
                    "Peak brightness",
                ],
                "the settings stand where the screen list would have"
            );
            // And they are that screen's settings, not a nameless set.
            assert_eq!(
                page[0].entries().unwrap()[1].setting(),
                Some(setting(intern(FIRST), DisplayValue::Hdr(true)))
            );
        });

        // A second capable screen brings the choice back.
        with_displays(&[(FIRST, capable(PEAK)), (SECOND, capable(PEAK))], || {
            let page = hdr_page();
            assert_eq!(
                page.iter().map(Entry::title).collect::<Vec<_>>(),
                [FIRST, SECOND]
            );
        });

        // A second screen that cannot do HDR does not: what decides the shape
        // is how many screens this page can actually offer, not how many are
        // plugged in. So this collapses too, onto the one that can.
        with_displays(
            &[(FIRST, capable(PEAK)), (SECOND, Support::default())],
            || {
                let hdr = hdr_row();
                assert!(hdr.comment().unwrap().starts_with(FIRST));
                assert_eq!(hdr.entries().unwrap()[0].title(), "HDR");
            },
        );
    }

    /// Displays that cannot do HDR are not listed, and a session where none
    /// can still opens on something that says why.
    #[test]
    fn only_capable_screens_are_listed() {
        let sdr = Support::default();
        with_displays(
            &[
                (FIRST, sdr),
                (SECOND, capable(PEAK)),
                (AWKWARD, capable(PEAK)),
            ],
            || {
                let screens = hdr_page();
                assert_eq!(
                    screens.iter().map(Entry::title).collect::<Vec<_>>(),
                    [SECOND, AWKWARD],
                    "the SDR screen is left out"
                );
            },
        );

        with_displays(&[(FIRST, sdr)], || {
            let screens = hdr_page();
            assert_eq!(screens.len(), 1, "never empty, or the bar cannot enter it");
            assert_eq!(screens[0].title(), "No display supports HDR");
            assert_eq!(screens[0].setting(), None, "and it is a reading");
            assert!(screens[0].entries().is_none(), "a dead end, not a column");
        });

        with_displays(&[], || {
            assert_eq!(hdr_page()[0].title(), "No display supports HDR");
        });
    }

    /// The HDR row itself, found by name rather than by position: what else
    /// the Display subcategory offers is not what these tests are about.
    fn hdr_row() -> Entry {
        display_row("HDR")
    }

    /// Whatever HDR opens onto: the screen list, the settings, or the reason
    /// there is neither.
    fn hdr_page() -> Vec<Entry> {
        hdr_row().entries().unwrap().to_vec()
    }

    /// The point of the whole restructure: a setting chosen on one screen
    /// belongs to that screen and leaves the others where they were.
    #[test]
    fn a_setting_belongs_to_the_screen_it_was_chosen_on() {
        with_displays(&[(FIRST, capable(PEAK)), (AWKWARD, capable(400))], || {
            let mut written = None;
            assert!(apply_with(
                setting(intern(FIRST), DisplayValue::Hdr(true)),
                |stored| written = Some(stored.display.clone()),
            ));
            assert!(apply_with(
                setting(intern(FIRST), DisplayValue::SdrBrightness(300)),
                |stored| written = Some(stored.display.clone()),
            ));

            assert_eq!(
                hdr_for(FIRST),
                Hdr {
                    enabled: true,
                    sdr_brightness: 300,
                    ..Hdr::default()
                }
            );
            assert_eq!(
                hdr_for(AWKWARD),
                Hdr::default(),
                "the other screen is untouched"
            );

            // And the file says the same thing, per screen.
            let written = written.expect("every setting is written down");
            assert_eq!(written[FIRST].hdr, Some(true));
            assert_eq!(written[FIRST].hdr_sdr_brightness, Some(300));
            assert!(
                !written.contains_key(AWKWARD),
                "a screen nobody changed is not written"
            );

            // The page rebuilt from those values marks what was chosen on
            // the screen it was chosen on, and only there.
            let marked = |screen: &str, control: usize| -> String {
                controls_for(screen)[control]
                    .entries()
                    .unwrap()
                    .iter()
                    .find(|entry| entry.chosen())
                    .unwrap()
                    .title()
                    .to_string()
            };
            assert_eq!(marked(FIRST, 0), "On");
            assert_eq!(marked(FIRST, 1), "300 cd/m²");
            assert_eq!(marked(AWKWARD, 0), "Off");
            assert_eq!(marked(AWKWARD, 1), "200 cd/m²");
        });
    }

    /// A control the hardware cannot honour says so instead of offering a
    /// choice that does nothing. Reported per screen, so a machine where one
    /// screen can and another cannot gets the right answer on each.
    #[test]
    fn a_control_with_nothing_behind_it_is_not_offered() {
        let no_gamut = Support {
            gamut: false,
            ..capable(PEAK)
        };
        with_displays(&[(FIRST, no_gamut), (SECOND, capable(PEAK))], || {
            let intensity = &controls_for(FIRST)[2];
            assert_eq!(intensity.title(), "sRGB color intensity");
            assert_eq!(intensity.comment(), Some("Not available on this display"));

            let inside = intensity.entries().expect("it still opens");
            assert_eq!(inside.len(), 1);
            assert_eq!(inside[0].setting(), None, "there is nothing to choose");
            assert!(inside[0].comment().unwrap().contains("degamma"));

            // The screen beside it, on the same page, still offers it.
            let offered = &controls_for(SECOND)[2];
            let values = offered.entries().unwrap();
            assert_eq!(values.len(), SRGB_INTENSITY.len());
            assert!(values.iter().all(|entry| entry.setting().is_some()));
        });
    }

    /// Choosing a value sets that value, for every value on every screen's
    /// page — a row named "250 cd/m²" that set 200 would be a setting nobody
    /// could trust.
    #[test]
    fn choosing_a_value_sets_that_value() {
        // Two screens, so the page keeps its screen list and the values are
        // reached the long way — the way they are on a multi-monitor desk.
        with_displays(&[(FIRST, capable(PEAK)), (SECOND, capable(PEAK))], || {
            for screen in [FIRST, SECOND] {
                for control in 0..4 {
                    let values = controls_for(screen)[control].entries().unwrap().to_vec();
                    for entry in &values {
                        let Some(chosen @ Setting::Display { display, value }) = entry.setting()
                        else {
                            panic!("{} sets nothing", entry.title());
                        };
                        assert_eq!(display, screen);
                        assert!(apply_with(chosen, |_| {}));

                        let live = hdr_for(screen);
                        match value {
                            DisplayValue::Hdr(on) => assert_eq!(live.enabled, on),
                            DisplayValue::SdrBrightness(nits) => {
                                assert_eq!(live.sdr_brightness, nits)
                            }
                            DisplayValue::SrgbIntensity(percent) => {
                                assert_eq!(live.srgb_intensity, percent)
                            }
                            DisplayValue::PeakBrightness(nits) => {
                                assert_eq!(live.peak_brightness, nits)
                            }
                            // Not on this page: the HDR controls are what
                            // `controls_for` walks, and neither half of a mode,
                            // nor the turn, nor a place is one of them.
                            DisplayValue::Resolution(_)
                            | DisplayValue::RefreshRate(_)
                            | DisplayValue::Orientation(_)
                            | DisplayValue::Place(_)
                            | DisplayValue::NightLight(_)
                            | DisplayValue::NightLightTemperature(_)
                            | DisplayValue::NightLightSchedule(_)
                            | DisplayValue::NightLightFrom(_)
                            | DisplayValue::NightLightUntil(_)
                            | DisplayValue::OledProtection(_) => {
                                unreachable!()
                            }
                        }

                        // And the row that is marked afterwards is this one.
                        let marked = controls_for(screen)[control]
                            .entries()
                            .unwrap()
                            .iter()
                            .find(|entry| entry.chosen())
                            .map(|entry| entry.setting());
                        assert_eq!(marked, Some(Some(chosen)));
                    }
                }
            }
        });
    }

    /// A list of values shows which one it is set to — one of them, and the
    /// one the shell is actually drawing with — and every row is drawn in the
    /// colour it stands for rather than in a description of it.
    #[test]
    fn the_colour_in_force_is_the_one_the_shell_draws_with() {
        theme::with_accent("Blue", || {
            let column = column();
            let colours = column[0].entries().unwrap()[0].entries().unwrap().to_vec();

            let chosen: Vec<&Entry> = colours.iter().filter(|entry| entry.chosen()).collect();
            assert_eq!(chosen.len(), 1, "exactly one value can be in force");
            assert_eq!(chosen[0].title(), "Blue");
            assert_eq!(
                chosen[0].swatch().map(Color::rgb),
                Some(theme::theme().accent.rgb())
            );

            for (entry, accent) in colours.iter().zip(theme::ACCENTS) {
                assert_eq!(entry.title(), accent.name);
                assert_eq!(entry.swatch(), Some(accent.theme.accent));
            }
        });
    }

    /// The Theme page's two halves, and what each of their rows does to the
    /// shell.
    ///
    /// Four things this holds, all of which have been wrong in another setting
    /// in this tree at some point: highlighting a row draws in that material
    /// without choosing it, walking back off the list puts the applied one back,
    /// choosing one writes the name the row is titled with rather than the name
    /// of the material that happened to be on screen — and, the one this page
    /// exists for, a row under one half leaves the other half exactly as it was.
    #[test]
    fn the_theme_rows_change_the_material_and_only_then_write_it_down() {
        let _held = WALLPAPER.lock().unwrap_or_else(|held| held.into_inner());
        let _put_back = WallpaperRestore::taken();
        let halves = || {
            appearance_page()[1]
                .entries()
                .expect("Theme opens onto its two halves")
                .to_vec()
        };
        let values = |half: usize| {
            halves()[half]
                .entries()
                .expect("each half opens onto its values")
                .to_vec()
        };
        for part in theme::PARTS {
            assert!(theme::set_style(part, "Default"));
        }

        assert_eq!(
            halves().iter().map(Entry::title).collect::<Vec<_>>(),
            ["Wallpaper", "Icons"],
            "the wallpaper first: it is the whole screen and the expensive half"
        );
        assert_eq!(
            halves().iter().map(Entry::icon).collect::<Vec<_>>(),
            [
                Some(crate::icons::SETTING_WALLPAPER),
                Some(crate::icons::SETTING_ICONS)
            ],
            "and each half wears the mark of the thing it changes"
        );

        for (half, part) in theme::PARTS.into_iter().enumerate() {
            let other = theme::PARTS[1 - half];
            let rows = values(half);
            assert_eq!(
                rows.iter().map(Entry::title).collect::<Vec<_>>(),
                part.styles().to_vec(),
                "the rows are the styles themselves, in the order the crate lists them"
            );
            assert_eq!(
                rows.len(),
                match part {
                    // The user's own picture, which is offered for the picture
                    // behind everything and has no meaning for a mark.
                    theme::Part::Wallpaper => 3,
                    theme::Part::Icons => 2,
                }
            );
            assert!(
                rows[0].chosen(),
                "a shell nobody has asked draws its own look"
            );
            assert!(!rows[1].chosen());
            assert!(
                rows.iter().all(|row| row.comment().is_some()),
                "a row about what a machine can afford has to say so"
            );

            // The two materials carry the *same* drawing — the mark of the half
            // they are under — and are told apart by being drawn in the
            // material each of them applies. The third row, where there is one,
            // is not a material and keeps the mark of a picture.
            let materials = &rows[..2];
            assert!(
                materials
                    .iter()
                    .all(|row| row.icon() == halves()[half].icon()),
                "both materials wear the mark of the thing they change"
            );
            assert_eq!(
                materials.iter().map(Entry::material).collect::<Vec<_>>(),
                [
                    Some(wallpaper::Style::Default),
                    Some(wallpaper::Style::Simple)
                ],
                "and each is drawn in the one it applies, which is all that \
                 tells one row from the other"
            );
            assert!(
                rows[2..].iter().all(|row| row.material().is_none()),
                "the user's own picture is a file and not a material"
            );

            // Highlighted: drawn in, not chosen.
            preview(rows[1].setting());
            assert_eq!(theme::style(part), wallpaper::Style::Simple);
            assert_eq!(
                theme::applied_style(part),
                wallpaper::Style::Default,
                "highlighting Simple is not choosing it"
            );
            assert_eq!(theme::style_flag(part), 1.0, "and the shader is told");
            assert_eq!(
                theme::style(other),
                wallpaper::Style::Default,
                "and told about this half alone"
            );

            // Walked off the list again.
            preview(None);
            assert_eq!(theme::style(part), wallpaper::Style::Default);
            assert_eq!(theme::style_flag(part), 0.0);

            // Chosen, and written down as itself.
            let mut persisted = None;
            assert!(apply_with(
                rows[1].setting().expect("Simple sets something"),
                |stored| {
                    persisted = Some((stored.theme_wallpaper.clone(), stored.theme_icons.clone()))
                },
            ));
            assert_eq!(theme::applied_style(part), wallpaper::Style::Simple);
            assert_eq!(
                theme::applied_style(other),
                wallpaper::Style::Default,
                "choosing one half is not choosing the other"
            );
            let (wallpaper_named, icons_named) = persisted.expect("both halves are written");
            assert_eq!(
                [wallpaper_named.as_deref(), icons_named.as_deref()],
                match part {
                    theme::Part::Wallpaper => [Some("Simple"), Some("Default")],
                    theme::Part::Icons => [Some("Default"), Some("Simple")],
                }
            );
            // And the page opens on it next time it is built.
            assert!(values(half)[1].chosen());

            assert!(theme::set_style(part, "Default"));
        }
    }

    /// The wallpaper half of the Theme setting is process-wide, and so is the
    /// file behind Custom wallpaper. Every test that moves either of them holds
    /// this, or two of them running at once would each be asserting about the
    /// other's shell.
    ///
    /// The same bargain [`theme::with_accent`] strikes for the accent, and taken
    /// through its own poison for the same reason: a test that panicked while
    /// holding it has already reported the failure that matters.
    static WALLPAPER: Mutex<()> = Mutex::new(());

    /// Put the wallpaper back the way the test found it, whatever happens in
    /// between.
    struct WallpaperRestore(Option<PathBuf>, wallpaper::Style);

    impl WallpaperRestore {
        fn taken() -> WallpaperRestore {
            WallpaperRestore(
                custom_wallpaper(),
                theme::applied_style(theme::Part::Wallpaper),
            )
        }
    }

    impl Drop for WallpaperRestore {
        fn drop(&mut self) {
            *CUSTOM_WALLPAPER.lock().unwrap() = self.0.take();
            theme::set_style(theme::Part::Wallpaper, self.1.name());
        }
    }

    /// The wallpaper's third row: the user's own picture.
    ///
    /// Four things, and each of them is a way this row is unlike every other
    /// value in the tree: it is offered for the wallpaper and never for the
    /// marks, it opens onto the disk instead of onto a list, it is markable all
    /// the same, and what it says under its title is which file was chosen.
    #[test]
    fn the_wallpaper_can_be_one_of_the_users_own_files() {
        let _held = WALLPAPER.lock().unwrap_or_else(|held| held.into_inner());
        let _put_back = WallpaperRestore::taken();
        let rows = || {
            appearance_page()[1].entries().expect("the two halves")[0]
                .entries()
                .expect("the wallpaper's values")
                .to_vec()
        };
        let custom = || {
            rows()
                .into_iter()
                .find(|row| row.title() == wallpaper::CUSTOM)
        };

        // Nothing chosen: the row invites rather than reports, and it is not
        // the one in force.
        *CUSTOM_WALLPAPER.lock().unwrap() = None;
        assert!(theme::set_style(theme::Part::Wallpaper, "Default"));
        let row = custom().expect("the wallpaper offers the user's own picture");
        assert!(!row.chosen());
        assert_eq!(row.comment(), Some("A picture or a film of your own"));
        assert_eq!(
            row.setting(),
            None,
            "there is no value here to apply; the file under it is the value"
        );

        // It opens on to the disk, walked to choose rather than to browse — the
        // same three places Files opens on.
        let Entry::Folder(folder) = &row else {
            panic!("the row is a way further in");
        };
        assert_eq!(
            folder.place,
            Some(crate::files::Place::Volumes(crate::files::Shows::Scenery))
        );

        // Chosen: marked, and saying which file.
        choose_custom_wallpaper_without_writing(Path::new("/home/somebody/Pictures/Sunset.jpg"));
        let row = custom().expect("still offered");
        assert!(row.chosen(), "the row a setting is set to carries the mark");
        assert_eq!(row.comment(), Some("Sunset.jpg"));
        assert_eq!(
            theme::applied_style(theme::Part::Wallpaper),
            wallpaper::Style::Custom
        );
        assert_eq!(
            theme::style_flag(theme::Part::Wallpaper),
            2.0,
            "and the shader is told to read the picture rather than draw a scene"
        );

        // The marks are never asked this question: there is nothing a
        // photograph could mean about the shape of an icon.
        let icons = appearance_page()[1].entries().expect("the two halves")[1]
            .entries()
            .expect("the marks' values")
            .to_vec();
        assert!(icons.iter().all(|row| row.title() != wallpaper::CUSTOM));
        assert!(!theme::set_style(theme::Part::Icons, wallpaper::CUSTOM));
        assert!(!theme::preview_style(theme::Part::Icons, wallpaper::CUSTOM));
        assert!(!theme::commit_style(theme::Part::Icons, wallpaper::CUSTOM));
    }

    /// Choosing a wallpaper without letting the test write the developer's own
    /// settings file. The two lines this leaves out are the save and the copy,
    /// and neither is what the rows above are about.
    fn choose_custom_wallpaper_without_writing(file: &Path) {
        *CUSTOM_WALLPAPER.lock().unwrap() = Some(file.to_path_buf());
        assert!(theme::commit_style(
            theme::Part::Wallpaper,
            wallpaper::CUSTOM
        ));
    }

    /// The file is written down beside the material, read back, and — the case
    /// this exists for — a file that is not there any more leaves the shell
    /// drawing its own wallpaper rather than nothing.
    #[test]
    fn a_wallpaper_that_is_not_there_falls_back_without_being_forgotten() {
        let _held = WALLPAPER.lock().unwrap_or_else(|held| held.into_inner());
        let _put_back = WallpaperRestore::taken();
        let kept = Stored {
            theme_wallpaper: Some(wallpaper::CUSTOM.to_string()),
            wallpaper_file: Some("/nowhere/at/all/sunset.jpg".to_string()),
            ..Stored::default()
        };
        *CUSTOM_WALLPAPER.lock().unwrap() = None;
        adopt_theme(&kept);
        assert_eq!(
            theme::applied_style(theme::Part::Wallpaper),
            wallpaper::Style::Default,
            "a picture that cannot be found is not a picture that can be drawn"
        );
        assert_eq!(custom_wallpaper(), None);

        // But the key itself survives the session, so a drive plugged back in
        // tomorrow brings the wallpaper with it. What is written is what the
        // file said, not what this shell could make of it.
        let written = toml::to_string(&kept).expect("the settings are writable as TOML");
        assert!(written.contains("wallpaper-file = \"/nowhere/at/all/sunset.jpg\""));

        // A file that is there is adopted, material and all.
        let dir = std::env::temp_dir().join(format!("lxb-wallpaper-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        let file = dir.join("wallpaper.png");
        std::fs::write(&file, b"x").expect("a file to point at");
        adopt_theme(&Stored {
            theme_wallpaper: Some(wallpaper::CUSTOM.to_string()),
            wallpaper_file: Some(file.display().to_string()),
            ..Stored::default()
        });
        assert_eq!(custom_wallpaper().as_deref(), Some(file.as_path()));
        assert_eq!(
            theme::applied_style(theme::Part::Wallpaper),
            wallpaper::Style::Custom
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The two keys the halves are written under, spelled once and asserted
    /// here.
    ///
    /// Neither of them is this shell's alone: the display manager reads both out
    /// of the file to bring a login screen up in the right material, and the
    /// compositor reads the wallpaper's for the frame it opens the session with.
    /// A rename that only the shell knew about would be a login screen quietly
    /// falling back to the default.
    #[test]
    fn the_two_halves_are_written_under_the_keys_they_say_they_are() {
        let written = toml::to_string(&Stored {
            theme_wallpaper: Some("Simple".to_string()),
            theme_icons: Some("Default".to_string()),
            ..Stored::default()
        })
        .expect("the settings are writable as TOML");
        assert_eq!(
            written.lines().take(2).collect::<Vec<_>>(),
            [
                r#"theme-wallpaper = "Simple""#,
                r#"theme-icons = "Default""#
            ],
            "the wallpaper first, beside the accent, in the order the page lists them"
        );
        assert!(
            !written.contains("\ntheme ="),
            "the key the two replaced is read and never written"
        );
        for part in theme::PARTS {
            assert!(
                written.contains(&format!("{} = ", part.key())),
                "{} is not written under {}",
                part.title(),
                part.key()
            );
        }
    }

    /// The one key both halves used to share is still read, and is read as what
    /// it meant: one answer about the whole shell.
    ///
    /// Without this a machine somebody deliberately stood down to Simple comes
    /// back up in the water after an update, which is the one thing this setting
    /// exists to prevent. A half with a key of its own outranks it, because that
    /// is a newer answer to a narrower question.
    #[test]
    fn a_file_from_before_the_split_still_says_what_it_said() {
        let _guard = LOCK.lock();
        for part in theme::PARTS {
            assert!(theme::set_style(part, "Default"));
        }

        adopt_theme(&Stored {
            theme: Some("Simple".to_string()),
            ..Stored::default()
        });
        for part in theme::PARTS {
            assert_eq!(
                theme::applied_style(part),
                wallpaper::Style::Simple,
                "{} ignored a file that named one material for the whole shell",
                part.title()
            );
        }

        adopt_theme(&Stored {
            theme: Some("Simple".to_string()),
            theme_icons: Some("Default".to_string()),
            ..Stored::default()
        });
        assert_eq!(
            theme::applied_style(theme::Part::Wallpaper),
            wallpaper::Style::Simple,
            "the half with nothing of its own keeps the old key's answer"
        );
        assert_eq!(
            theme::applied_style(theme::Part::Icons),
            wallpaper::Style::Default,
            "and the half that has been answered since outranks it"
        );

        for part in theme::PARTS {
            assert!(theme::set_style(part, "Default"));
        }
    }

    /// Every row carries what choosing it does, and it does what the row says
    /// it does — a swatch named for one colour that set another would be the
    /// one setting in the shell nobody could trust.
    #[test]
    fn choosing_a_colour_sets_that_colour() {
        theme::with_accent("Purple", || {
            let column = column();
            let colours = column[0].entries().unwrap()[0].entries().unwrap().to_vec();

            for entry in &colours {
                let Entry::Choice(choice) = entry else {
                    panic!("a colour is a value, not {entry:?}");
                };
                let Some(setting @ Setting::Accent(name)) = choice.setting else {
                    panic!("{} sets nothing", choice.title);
                };
                assert_eq!(name, choice.title);

                // Walk onto the row before Apply, just as the shell does. The
                // persistence callback is replaced so a test never touches
                // the config of whoever is running it.
                preview(Some(setting));
                theme::animate(1.0);
                let mut persisted = None;
                assert!(apply_with(setting, |stored| {
                    persisted = stored.accent.clone();
                }));
                assert_eq!(persisted.as_deref(), Some(name));
                assert_eq!(theme::accent().name, name);
                assert_eq!(theme::theme().accent.rgb(), choice.swatch.unwrap().rgb());
                assert_eq!(setting, Setting::Accent(name));
            }
        });
    }

    #[test]
    fn preview_is_temporary_and_apply_changes_what_back_restores() {
        theme::with_accent("Purple", || {
            preview(Some(Setting::Accent("Green")));
            assert_eq!(theme::accent().name, "Purple");
            theme::animate(1.0);
            assert_eq!(theme::theme().accent.rgb(), theme::GREEN.accent.rgb());

            let mut persisted = Vec::new();
            assert!(apply_with(Setting::Accent("Green"), |stored| {
                persisted.push(stored.accent.clone().unwrap());
            }));
            assert_eq!(persisted, ["Green".to_string()]);
            assert_eq!(theme::accent().name, "Green");

            preview(Some(Setting::Accent("Red")));
            theme::animate(1.0);
            assert_eq!(theme::accent().name, "Green", "Red is only highlighted");
            assert_eq!(theme::theme().accent.rgb(), theme::RED.accent.rgb());

            preview(None);
            assert_eq!(
                theme::theme().accent.rgb(),
                theme::RED.accent.rgb(),
                "Back begins at the colour currently on screen"
            );
            theme::animate(1.0);
            assert_eq!(theme::theme().accent.rgb(), theme::GREEN.accent.rgb());
        });
    }

    /// Highlighting an HDR value must not reconfigure a connector: the screen
    /// would cut to black every time the cursor moved down the list. It is
    /// still allowed to take the accent back off preview, which is what
    /// leaving a colour list by any other route does.
    #[test]
    fn walking_over_a_display_value_changes_nothing() {
        with_displays(&[(FIRST, capable(PEAK))], || {
            theme::with_accent("Purple", || {
                let before = hdr_for(FIRST);
                let dp = intern(FIRST);
                preview(Some(setting(dp, DisplayValue::Hdr(true))));
                preview(Some(setting(dp, DisplayValue::SdrBrightness(400))));
                preview(Some(setting(dp, DisplayValue::SrgbIntensity(100))));
                preview(Some(setting(dp, DisplayValue::PeakBrightness(4000))));
                assert_eq!(
                    hdr_for(FIRST),
                    before,
                    "nothing is asked for until it is chosen"
                );
                theme::animate(1.0);
                assert_eq!(theme::accent().name, "Purple");
            });
        });
    }

    /// What is written is what is read, per screen — and the keys are the ones
    /// the preamble documents.
    #[test]
    fn the_settings_survive_the_file() {
        let written = Stored {
            accent: Some("Blue".to_string()),
            display: BTreeMap::from([(
                FIRST.to_string(),
                StoredDisplay {
                    mode: Some("2560x1440@144".to_string()),
                    transform: Some("90".to_string()),
                    order: Some(2),
                    hdr: Some(true),
                    hdr_sdr_brightness: Some(250),
                    hdr_srgb_intensity: Some(50),
                    hdr_peak_brightness: Some(1000),
                    night_light: Some(true),
                    night_light_temperature: Some(3400),
                    night_light_schedule: Some("hours".to_string()),
                    night_light_from: Some(21),
                    night_light_until: Some(7),
                    oled_protection: Some(true),
                },
            )]),
            media_sort: BTreeMap::from([
                ("Images".to_string(), "created-newest-first".to_string()),
                ("Music".to_string(), "type".to_string()),
            ]),
            steam_sort: Some("last-played".to_string()),
            ..Stored::default()
        };
        let written_out = toml::to_string_pretty(&written).unwrap();
        let body = format!("{PREAMBLE}{written_out}");
        assert_eq!(toml::from_str::<Stored>(&body).unwrap(), written);
        // One section per connector. The name goes in bare where TOML allows
        // it, so the quoting is not pinned here — only that the section is
        // keyed by whatever the connector is called.
        assert!(body.contains(&format!("[display.{FIRST}]")), "{body}");
        // And a name that does need quoting still survives the trip, because
        // nothing here may assume a connector is named tidily.
        let awkward = Stored {
            display: BTreeMap::from([(AWKWARD.to_string(), StoredDisplay::default())]),
            ..Stored::default()
        };
        let quoted = toml::to_string_pretty(&awkward).unwrap();
        assert_eq!(toml::from_str::<Stored>(&quoted).unwrap(), awkward);
        for key in [
            "mode",
            "transform",
            "hdr-sdr-brightness",
            "hdr-srgb-intensity",
            "hdr-peak-brightness",
            "oled-protection",
        ] {
            assert!(body.contains(key), "{key} is not written under that name");
        }

        with_displays(&[(FIRST, capable(0))], || {
            adopt(toml::from_str(&body).unwrap());
            assert_eq!(
                hdr_for(FIRST),
                Hdr {
                    enabled: true,
                    sdr_brightness: 250,
                    srgb_intensity: 50,
                    peak_brightness: 1000,
                }
            );
            assert_eq!(mode_for(FIRST), Some(mode(2560, 1440, 144)));
            // And the switch that rests this screen while another one is
            // being used, which is written and read like anything else per
            // display — see [`oled_protection_for`].
            assert!(oled_protection_for(FIRST));
            assert!(
                !oled_protection_for(SECOND),
                "a screen the file says nothing about is never rested"
            );
            // The order each shelf is listed in comes back with the rest of
            // it, and a shelf the file says nothing about is left alphabetical
            // rather than given somebody else's answer.
            assert_eq!(
                media_sort(crate::media::Kind::Audio),
                Some(crate::media::Sort::Type)
            );
            assert_eq!(
                media_sort(crate::media::Kind::Image),
                Some(crate::media::Sort::NewestFirst)
            );
            assert_eq!(media_sort(crate::media::Kind::Video), None);
            // And the one order the Steam column is listed in, which is a bare
            // key rather than a shelf.
            assert_eq!(
                steam_sort(),
                Some(lxb_steam::library::Sort::RecentlyPlayedFirst)
            );
        });
        assert!(body.contains("[media-sort]"), "{body}");
        // Written above both tables, because everything below a table header
        // belongs to that table: a bare key written after them would come back
        // as a shelf called `steam-sort`. Against the written file rather than
        // the whole body, the preamble having a good deal to say about
        // `[display.NAME]` before any of it is written.
        let (top, tables) = written_out
            .split_once("[display.")
            .expect("the display table");
        assert!(top.contains("steam-sort = \"last-played\""), "{body}");
        assert!(!tables.contains("steam-sort"), "{body}");

        // An order this shell does not have is ignored rather than refused:
        // the file is one the user is entitled to open and edit.
        //
        // Inside `with_displays`, empty though it is, because `adopt` replaces
        // every display setting in the process — a bare call here is a second
        // test wiping the one running beside it, which is exactly what a shared
        // `static` and a parallel runner do to each other.
        with_displays(&[], || {
            adopt(Stored {
                media_sort: BTreeMap::from([("Music".to_string(), "by vibes".to_string())]),
                steam_sort: Some("by vibes".to_string()),
                ..Stored::default()
            });
            assert_eq!(media_sort(crate::media::Kind::Audio), None);
            assert_eq!(steam_sort(), None);
            // Ignored, but not thrown away: the next thing that writes the file
            // must not quietly delete a choice made by a later version of the
            // shell than this one.
            assert_eq!(stored().steam_sort.as_deref(), Some("by vibes"));
        });

        // A file cut down to nothing still parses, and says nothing.
        assert_eq!(toml::from_str::<Stored>("").unwrap(), Stored::default());
    }

    /// How loud the shell's effects and Start music are survives a session,
    /// and a hand-typed level outside the range the mixer row can reach is
    /// brought inside it rather than refusing the file.
    ///
    /// Never through [`set_sound`], which writes to the config directory of
    /// whoever is running the tests. What is exercised here is the pair that
    /// decides what lands in the file and what comes back out of it.
    #[test]
    fn the_shell_s_own_volume_is_remembered() {
        with_displays(&[], || {
            adopt(Stored {
                sound_volume: Some(0.35),
                sound_muted: Some(true),
                ..Stored::default()
            });
            assert_eq!(
                sound(),
                Level {
                    value: 0.35,
                    muted: true
                }
            );

            let written = stored();
            assert_eq!(written.sound_volume, Some(0.35));
            assert_eq!(written.sound_muted, Some(true));
            let body = toml::to_string_pretty(&written).unwrap();
            assert!(body.contains("sound-volume"), "{body}");
            assert!(body.contains("sound-muted"), "{body}");

            adopt(Stored {
                sound_volume: Some(4.0),
                ..Stored::default()
            });
            assert_eq!(sound().value, 1.0, "a level past the top of the row");

            // A file that says nothing about the sound leaves it where the
            // session already had it, rather than answering for the user.
            adopt(Stored::default());
            assert_eq!(sound().value, 1.0);
        });
    }

    /// Settings > Sounds > Start music, walked the way the shell walks it: the
    /// page opens marking what the session is doing, choosing the other row
    /// turns the music over, and the page rebuilt afterwards says so.
    ///
    /// Never through [`apply`], for the reason the volume test gives: that one
    /// writes to the config directory of whoever is running the tests.
    #[test]
    fn the_start_music_switch_turns_the_music_over() {
        with_displays(&[], || {
            *START_MUSIC.lock().unwrap() = true;

            let page = start_music_page();
            assert_eq!(
                page.iter().map(Entry::title).collect::<Vec<_>>(),
                ["Off", "On"],
                "the switch every other switch in this tree is"
            );
            assert!(page[1].chosen(), "the page opens on what is playing");
            assert!(!page[0].chosen());

            // Walking onto the other row is an invitation to look, not a
            // decision: the music the user is listening to keeps playing until
            // they press something.
            preview(page[0].setting());
            assert!(start_music(), "highlighting Off is not choosing it");

            let mut persisted = None;
            assert!(apply_with(
                page[0].setting().expect("Off sets something"),
                |stored| persisted = stored.start_music
            ));
            assert!(!start_music());
            assert_eq!(persisted, Some(false), "and it is written down");

            let page = start_music_page();
            assert!(page[0].chosen(), "the mark has moved with it");
            assert!(!page[1].chosen());

            assert!(apply_with(
                page[1].setting().expect("On sets something"),
                |_| {}
            ));
            assert!(start_music());
        });
    }

    /// A machine with no battery is offered neither the mark nor the row that
    /// would turn its figures on, and a machine with one is offered both.
    ///
    /// The point of the whole arrangement. A desktop given this switch would be
    /// given a setting it can never see the effect of, which is worse than not
    /// being given it: somebody would turn it on and go looking for what
    /// changed.
    #[test]
    fn the_battery_row_stands_only_on_a_machine_that_has_a_battery() {
        with_battery(None, || {
            assert_eq!(
                appearance_page()
                    .iter()
                    .map(Entry::title)
                    .collect::<Vec<_>>(),
                ["Accent color", "Theme"],
                "a machine with no battery is offered a battery setting",
            );
        });

        with_battery(
            Some(crate::power::Charge {
                percent: 72,
                charging: false,
            }),
            || {
                let page = appearance_page();
                assert_eq!(
                    page.iter().map(Entry::title).collect::<Vec<_>>(),
                    ["Accent color", "Theme", "Battery percentage"],
                    "the accent first: it is the whole shell, and this is one mark",
                );
                // The row is drawn at the level the machine is actually at, so
                // the list is headed by the mark the user is deciding about.
                assert_eq!(page[2].icon(), Some(crate::icons::BATTERY_HIGH));
            },
        );
    }

    /// The switch turns the figures over, and highlighting the other row is an
    /// invitation to look rather than a decision — the boundary every other
    /// switch in this tree is under.
    ///
    /// Never through [`apply`], for the reason the volume test gives: that one
    /// writes to the config directory of whoever is running the tests.
    #[test]
    fn the_battery_percentage_switch_turns_the_figures_over() {
        with_battery(
            Some(crate::power::Charge {
                percent: 96,
                charging: false,
            }),
            || {
                *BATTERY_PERCENT.lock().unwrap() = false;

                let page = |()| {
                    appearance_page()[2]
                        .entries()
                        .expect("Battery percentage opens onto its two values")
                        .to_vec()
                };
                let rows = page(());
                assert_eq!(
                    rows.iter().map(Entry::title).collect::<Vec<_>>(),
                    ["Off", "On"],
                    "the switch every other switch in this tree is",
                );
                assert!(rows[0].chosen(), "a shell nobody has asked opens on Off");
                assert!(!rows[1].chosen());

                preview(rows[1].setting());
                assert!(!battery_percent(), "highlighting On is not choosing it",);

                let mut persisted = None;
                assert!(apply_with(
                    rows[1].setting().expect("On sets something"),
                    |stored| persisted = stored.battery_percent,
                ));
                assert!(battery_percent());
                assert_eq!(persisted, Some(true), "and it is written down");

                let rows = page(());
                assert!(rows[1].chosen(), "the mark has moved with it");
                assert!(!rows[0].chosen());

                assert!(apply_with(
                    rows[0].setting().expect("Off sets something"),
                    |_| {},
                ));
                assert!(!battery_percent());
            },
        );
    }

    /// The start screen says what its buttons do until somebody turns it off,
    /// and the switch under System is what turns it.
    ///
    /// Never through [`apply`], for the reason the volume test gives: that one
    /// writes to the config directory of whoever is running the tests.
    #[test]
    fn the_button_hints_switch_turns_the_legend_over() {
        with_displays(&[], || {
            // Where a session that has never been asked has it — see the
            // argument on [`BUTTON_HINTS`], which is where the default is
            // declared and where it belongs.
            *BUTTON_HINTS.lock().unwrap() = true;

            let page = || {
                system_page()
                    .into_iter()
                    .find(|entry| entry.title() == "Button hints")
                    .expect("the System page offers the button hints")
                    .entries()
                    .expect("Button hints opens onto its two values")
                    .to_vec()
            };
            let rows = page();
            assert_eq!(
                rows.iter().map(Entry::title).collect::<Vec<_>>(),
                ["Off", "On"],
                "the switch every other switch in this tree is",
            );
            assert!(rows[1].chosen(), "and a shell nobody has asked opens on On");
            assert!(!rows[0].chosen());

            preview(rows[0].setting());
            assert!(button_hints(), "highlighting Off is not choosing it");

            let mut persisted = None;
            assert!(apply_with(
                rows[0].setting().expect("Off sets something"),
                |stored| persisted = stored.button_hints,
            ));
            assert!(!button_hints());
            assert_eq!(persisted, Some(false), "and it is written down");

            let rows = page();
            assert!(rows[0].chosen(), "the mark has moved with it");
            assert!(!rows[1].chosen());

            // Off survives being written out and read back, which is the whole
            // point of writing it down: nobody turns this off meaning "until I
            // next start the shell".
            let body = toml::to_string_pretty(&stored()).unwrap();
            assert!(body.contains("button-hints"), "{body}");
            *BUTTON_HINTS.lock().unwrap() = true;
            adopt(toml::from_str(&body).unwrap());
            assert!(!button_hints(), "and it comes back off");

            // A file that says nothing about it changes nothing — which is how
            // a session whose settings were written before this key existed
            // keeps the hints it has always had. See [`adopt`].
            assert_eq!(Stored::default().button_hints, None);
            adopt(Stored::default());
            assert!(
                !button_hints(),
                "a silent file answers nothing for the user"
            );
            *BUTTON_HINTS.lock().unwrap() = true;
            adopt(Stored::default());
            assert!(button_hints(), "in either direction");

            assert!(apply_with(
                page()[0].setting().expect("Off sets something"),
                |_| {},
            ));
            assert!(!button_hints());
        });
    }

    /// Whether the charge is written out survives a session, and comes back off
    /// on a machine that has never been asked.
    ///
    /// Written on every machine, a desktop included: a file carried from the
    /// laptop it was set on must not lose the switch on a machine that has no
    /// row for it. See the `battery-percent` key in [`PREAMBLE`].
    #[test]
    fn whether_the_charge_is_written_out_is_remembered() {
        with_displays(&[], || {
            *BATTERY_PERCENT.lock().unwrap() = false;

            adopt(Stored {
                battery_percent: Some(true),
                ..Stored::default()
            });
            assert!(battery_percent());

            let written = stored();
            assert_eq!(written.battery_percent, Some(true));
            let body = toml::to_string_pretty(&written).unwrap();
            assert!(body.contains("battery-percent"), "{body}");

            adopt(toml::from_str(&body).unwrap());
            assert!(battery_percent(), "and it comes back on");

            adopt(Stored::default());
            assert!(
                battery_percent(),
                "a silent file answers nothing for the user",
            );

            // And a machine that has never had one written comes up without
            // figures: the mark says how much is left, which is what a glance
            // at a corner asks.
            *BATTERY_PERCENT.lock().unwrap() = false;
            assert!(!battery_percent());
        });
    }

    /// A battery that only moved a point does not rebuild the Settings column,
    /// and one that crossed into another drawing does.
    ///
    /// The reason [`note_battery`] compares what the row *shows* rather than
    /// what it was told: a discharging battery reports a different number every
    /// time it is read, and a column rebuilt three times a minute for the
    /// length of a session would be rebuilt for a difference it does not draw.
    #[test]
    fn only_a_battery_that_changed_the_drawing_rebuilds_the_column() {
        let charge = |percent, charging| Some(crate::power::Charge { percent, charging });
        with_battery(None, || {
            assert!(note_battery(charge(72, false)), "a battery arrived");
            assert!(
                !note_battery(charge(71, false)),
                "one point is not a drawing"
            );
            assert!(!note_battery(charge(60, false)), "nor is the whole band");
            assert!(note_battery(charge(59, false)), "but its edge is");
            assert!(
                note_battery(charge(59, true)),
                "and so is being plugged in, at the same charge",
            );
            assert!(note_battery(None), "and so is the battery going away");
            assert!(!note_battery(None));
        });
    }

    /// Do not disturb survives a session, and a file that says nothing about
    /// it leaves the switch where the shell has it — which on the ordinary
    /// first run is off.
    ///
    /// It is remembered at all because it is a switch somebody threw on
    /// purpose: a console that quietly turned it back off overnight would
    /// deliver a night of announcements at breakfast, which is the one thing
    /// the switch was thrown to prevent.
    ///
    /// Never through [`set_do_not_disturb`], which writes to the config
    /// directory of whoever is running the tests — the same reason
    /// [`the_shell_s_own_volume_is_remembered`] goes through this pair.
    #[test]
    fn whether_anything_may_interrupt_is_remembered() {
        with_displays(&[], || {
            *DO_NOT_DISTURB.lock().unwrap() = false;

            adopt(Stored {
                do_not_disturb: Some(true),
                ..Stored::default()
            });
            assert!(do_not_disturb());

            let written = stored();
            assert_eq!(written.do_not_disturb, Some(true));
            let body = toml::to_string_pretty(&written).unwrap();
            assert!(body.contains("do-not-disturb"), "{body}");

            adopt(toml::from_str(&body).unwrap());
            assert!(do_not_disturb(), "and it comes back on");

            adopt(Stored::default());
            assert!(
                do_not_disturb(),
                "a silent file answers nothing for the user"
            );

            // A machine that has never had one written comes up able to be
            // interrupted: a shell that arrived refusing to show what the
            // machine had to say would look like one whose notifications are
            // broken.
            *DO_NOT_DISTURB.lock().unwrap() = false;
            assert!(!do_not_disturb());
            assert_eq!(Stored::default().do_not_disturb, None);
        });
    }

    /// Which control the user reaches for survives a session, which is the
    /// whole reason it is written down: somebody who spent last night typing
    /// does not become a controller user again by turning the machine off, and
    /// a console that forgot would throw a keyboard over the first text field
    /// of every morning.
    ///
    /// Through [`adopt`] and [`stored`] rather than [`set_controller_in_hand`],
    /// which writes to the config directory of whoever is running the tests —
    /// the same reason the switch above goes through this pair.
    #[test]
    fn which_control_is_in_hand_is_remembered() {
        with_displays(&[], || {
            *CONTROLLER_IN_HAND.lock().unwrap() = true;

            adopt(Stored {
                controller_in_hand: Some(false),
                ..Stored::default()
            });
            assert!(!controller_in_hand());

            let written = stored();
            assert_eq!(written.controller_in_hand, Some(false));
            let body = toml::to_string_pretty(&written).unwrap();
            assert!(body.contains("controller-in-hand"), "{body}");

            adopt(toml::from_str(&body).unwrap());
            assert!(
                !controller_in_hand(),
                "the keyboard was forgotten overnight"
            );

            adopt(Stored::default());
            assert!(
                !controller_in_hand(),
                "a silent file answers nothing for the user"
            );

            // A machine nobody's habits have been recorded on yet is a console,
            // and a console is held with a pad: everything the shell offers one
            // is offered until somebody types.
            *CONTROLLER_IN_HAND.lock().unwrap() = true;
            assert!(controller_in_hand());
            assert_eq!(Stored::default().controller_in_hand, None);
        });
    }

    /// Whether the Start screen plays anything survives a session, and a file
    /// that says nothing about it leaves the music where the shell has it —
    /// which on the ordinary first run is playing.
    #[test]
    fn whether_the_start_music_plays_is_remembered() {
        with_displays(&[], || {
            *START_MUSIC.lock().unwrap() = true;

            adopt(Stored {
                start_music: Some(false),
                ..Stored::default()
            });
            assert!(!start_music());

            let written = stored();
            assert_eq!(written.start_music, Some(false));
            let body = toml::to_string_pretty(&written).unwrap();
            assert!(body.contains("start-music"), "{body}");

            adopt(toml::from_str(&body).unwrap());
            assert!(!start_music(), "and it comes back off");

            adopt(Stored::default());
            assert!(!start_music(), "a silent file answers nothing for the user");

            // And it says nothing about how loud the rest of the shell is: two
            // questions, two keys, and neither answers the other.
            let level = Level {
                value: 0.4,
                muted: false,
            };
            *SOUND.lock().unwrap() = level;
            adopt(Stored {
                start_music: Some(true),
                ..Stored::default()
            });
            assert!(start_music());
            assert_eq!(sound(), level);
        });
    }

    /// The two values under Settings > Sounds > Start music.
    fn start_music_page() -> Vec<Entry> {
        device_row("Start music")
            .entries()
            .expect("Start music opens onto its two values")
            .to_vec()
    }

    /// Every device the machine has is a row, the one it is using is marked,
    /// and the row above the list says which that is without stepping in.
    #[test]
    fn the_device_pages_list_the_machine_and_mark_what_it_is_using() {
        let listing = Devices {
            outputs: vec![
                device(
                    "test_output.speakers",
                    "Test Audio Controller",
                    Some("Digital Stereo (Test 1)"),
                    false,
                ),
                device(
                    "test_output.headset",
                    "Test Wireless Headset",
                    Some("Analog Stereo"),
                    true,
                ),
            ],
            inputs: vec![device(
                "test_input.microphone",
                "Test Microphone",
                Some("Mono"),
                true,
            )],
            server: true,
        };
        with_devices(listing, || {
            // The devices first, and the shell's own sounds after them: a
            // session is set up before it is decorated.
            assert_eq!(
                sounds_page().iter().map(Entry::title).collect::<Vec<_>>(),
                ["Output device", "Input device", "Start music"]
            );

            let outputs = device_row("Output device");
            assert_eq!(
                outputs.comment(),
                Some("Test Wireless Headset — Analog Stereo"),
                "the row above the list answers the question without opening it"
            );
            let page = outputs.entries().expect("it opens onto the devices");
            assert_eq!(
                page.iter().map(Entry::title).collect::<Vec<_>>(),
                ["Test Audio Controller", "Test Wireless Headset"]
            );
            // The card on the line the eye lands on, how it is being driven
            // under it.
            assert_eq!(page[0].comment(), Some("Digital Stereo (Test 1)"));
            assert!(!page[0].chosen());
            assert!(page[1].chosen(), "the one the machine is using");

            let inputs = device_row("Input device");
            assert_eq!(inputs.comment(), Some("Test Microphone — Mono"));
            assert_eq!(
                inputs
                    .entries()
                    .expect("it opens onto the devices")
                    .iter()
                    .map(Entry::title)
                    .collect::<Vec<_>>(),
                ["Test Microphone"]
            );

            // Highlighting a device is looking at its name. Moving every sound
            // on the machine to it as the cursor passes over would be a preview
            // of somebody's film arriving in the wrong room.
            preview(page[0].setting());
            assert!(
                DEVICES.lock().unwrap().outputs[1].default,
                "nothing moved for a highlight"
            );
        });
    }

    /// Choosing one is a message to the sound server and nothing else: the
    /// shell does not write it down, because the server is what remembers it and
    /// a second copy here would undo a device chosen in any other mixer at the
    /// next login.
    #[test]
    fn choosing_a_device_is_told_to_the_server_and_written_nowhere() {
        let listing = Devices {
            outputs: vec![
                device("test_output.speakers", "Test Speakers", None, true),
                device("test_output.headset", "Test Headset", None, false),
            ],
            inputs: Vec::new(),
            server: true,
        };
        with_devices(listing, || {
            let page = device_row("Output device")
                .entries()
                .expect("it opens onto the devices")
                .to_vec();
            let chosen = page[1].setting().expect("a device row sets something");
            assert_eq!(
                chosen,
                Setting::SoundDevice {
                    direction: Direction::Output,
                    id: "test_output.headset",
                }
            );

            let mut written = false;
            assert!(apply_with(chosen, |_| written = true));
            assert!(!written, "nothing about a device belongs in the file");

            // Nor does it appear in what the file would be if something else
            // were written a moment later.
            let body = toml::to_string_pretty(&stored()).unwrap();
            assert!(!body.contains("device"), "{body}");

            // And the mark has not moved here: the listing is the sound
            // server's answer, and this module does not edit it on the way
            // past. What moves it is the press reaching
            // `system::Quick::use_device`, which is where that is tested.
            let page = device_row("Output device")
                .entries()
                .expect("it opens onto the devices")
                .to_vec();
            assert!(page[0].chosen());
            assert!(!page[1].chosen());
        });
    }

    /// A page with nothing on it is a row that does nothing when pressed, so
    /// there is always something — and the two ways of having no device are not
    /// the same fact.
    #[test]
    fn a_machine_with_no_devices_says_which_kind_of_nothing_it_has() {
        with_devices(Devices::none(), || {
            let page = device_row("Output device")
                .entries()
                .expect("it opens onto something")
                .to_vec();
            assert_eq!(page.len(), 1);
            assert_eq!(page[0].title(), "No sound server is running");
            assert!(
                page[0].setting().is_none(),
                "an explanation is not a value to choose"
            );
            assert!(!page[0].chosen());
            assert_eq!(
                device_row("Output device").comment(),
                Some("Where everything on the machine plays")
            );
        });

        // A server that answers and lists nothing is a machine with no sound
        // card, which is something else entirely.
        with_devices(
            Devices {
                server: true,
                ..Devices::none()
            },
            || {
                let outputs = device_row("Output device");
                let page = outputs.entries().expect("it opens onto something");
                assert_eq!(page[0].title(), "No output device");
                let inputs = device_row("Input device");
                let page = inputs.entries().expect("it opens onto something");
                assert_eq!(page[0].title(), "No input device");
            },
        );
    }

    /// The sound server names a device that is not one of the rows — an
    /// output's monitor chosen as the input somewhere else, which the input
    /// page deliberately does not list. Nothing is marked, and the row above
    /// says what the page is for rather than inventing an answer.
    #[test]
    fn a_device_the_page_does_not_list_marks_nothing() {
        with_devices(
            Devices {
                inputs: vec![device(
                    "test_input.microphone",
                    "Test Microphone",
                    None,
                    false,
                )],
                server: true,
                ..Devices::none()
            },
            || {
                let inputs = device_row("Input device");
                assert_eq!(
                    inputs.comment(),
                    Some("What everything on the machine records from")
                );
                let page = inputs.entries().expect("it opens onto the devices");
                assert_eq!(page.len(), 1);
                assert!(!page[0].chosen());
            },
        );
    }

    /// A file written before this page had a screen list still says what its
    /// author wanted, so it becomes the starting point for every screen rather
    /// than being dropped on the floor.
    #[test]
    fn settings_from_before_the_screen_list_carry_over() {
        let old = "\
accent = \"Green\"
hdr = true
hdr-sdr-brightness = 250
hdr-srgb-intensity = 75
hdr-peak-brightness = 600
";
        with_displays(&[(FIRST, capable(PEAK)), (SECOND, capable(0))], || {
            adopt(toml::from_str(old).unwrap());
            let inherited = Hdr {
                enabled: true,
                sdr_brightness: 250,
                srgb_intensity: 75,
                peak_brightness: 600,
            };
            assert_eq!(hdr_for(FIRST), inherited);
            assert_eq!(hdr_for(SECOND), inherited);

            // Changing one screen takes it off the shared starting point and
            // leaves the other where it was.
            assert!(apply_with(
                setting(intern(FIRST), DisplayValue::SdrBrightness(100)),
                |stored| {
                    // The screen that changed gets a section of its own, and
                    // the inherited values keep being written — otherwise the
                    // screens still relying on them would fall back to the
                    // shell's defaults at the next startup, silently, because
                    // the file had stopped saying anything about them.
                    assert_eq!(stored.display[FIRST].hdr_sdr_brightness, Some(100));
                    assert_eq!(stored.hdr, Some(true));
                    assert_eq!(stored.hdr_sdr_brightness, Some(250));
                    assert_eq!(stored.hdr_srgb_intensity, Some(75));
                    assert_eq!(stored.hdr_peak_brightness, Some(600));
                }
            ));
            assert_eq!(hdr_for(FIRST).sdr_brightness, 100);
            assert_eq!(hdr_for(FIRST).srgb_intensity, 75, "the rest carried over");
            assert_eq!(hdr_for(SECOND), inherited);

            // And reading that file back leaves both screens where they were,
            // which is the whole point of still writing the defaults.
            let written = toml::to_string_pretty(&stored()).unwrap();
            adopt(toml::from_str(&written).unwrap());
            assert_eq!(hdr_for(FIRST).sdr_brightness, 100);
            assert_eq!(hdr_for(SECOND), inherited);
            // Including a screen nobody has ever touched.
            assert_eq!(hdr_for("TEST-OUT-NEVER-SEEN"), inherited);
        });
    }

    /// A display keeps its settings while it is unplugged: the shell files
    /// them under the connector, and the compositor does the same.
    #[test]
    fn an_unplugged_screen_keeps_its_settings() {
        with_displays(&[(FIRST, capable(PEAK)), (SECOND, capable(PEAK))], || {
            assert!(apply_with(
                setting(intern(FIRST), DisplayValue::Hdr(true)),
                |_| {}
            ));
            assert_eq!(
                hdr_page().iter().map(Entry::title).collect::<Vec<_>>(),
                [FIRST, SECOND]
            );

            // One unplugged, and with a single screen left the list it was in
            // goes with it — the page collapses onto the survivor.
            note_support(vec![(SECOND.to_string(), capable(PEAK))]);
            assert!(hdr_row().comment().unwrap().starts_with(SECOND));
            assert!(hdr_for(FIRST).enabled, "but its settings are not forgotten");

            // Both unplugged.
            note_support(vec![]);
            assert_eq!(hdr_page()[0].title(), "No display supports HDR");
            assert!(hdr_for(FIRST).enabled);

            // And back, still set the way it was left.
            note_support(vec![
                (FIRST.to_string(), capable(PEAK)),
                (SECOND.to_string(), capable(PEAK)),
            ]);
            let switch = &controls_for(FIRST)[0];
            assert!(switch.entries().unwrap()[1].chosen(), "still On");
        });
    }

    /// Interning is what lets a setting name its display and stay `Copy`. Two
    /// rows built for the same connector have to compare equal, or a value
    /// chosen on one would not mark the row it came from.
    #[test]
    fn a_display_name_interns_to_one_string() {
        let once = intern(FIRST);
        // The same name arriving as a fresh allocation, which is how it
        // arrives in practice: a `String` off the Wayland connection.
        let again = intern(&String::from(FIRST));
        assert_eq!(once.as_ptr(), again.as_ptr());
        assert_ne!(intern(SECOND).as_ptr(), once.as_ptr());
        // Including a name that is not a tidy identifier.
        assert_eq!(intern(AWKWARD), AWKWARD);
        assert_eq!(
            setting(once, DisplayValue::Hdr(true)),
            setting(again, DisplayValue::Hdr(true))
        );
    }

    // -----------------------------------------------------------------------
    // resolution and refresh rate
    // -----------------------------------------------------------------------

    /// Whatever one of the two mode pages opens onto: the screen list, the
    /// values, or the reason there is neither.
    fn page(title: &str) -> Vec<Entry> {
        display_row(title).entries().unwrap().to_vec()
    }

    /// The values of one screen on one of those pages, however the page
    /// reaches them — which is one step shorter when only one screen has a
    /// choice, exactly as it is on the HDR page.
    fn values_for(title: &str, name: &str) -> Vec<Entry> {
        let page = page(title);
        // The screen list is a list of folders; the values are choices. Which
        // shape came back is what says whether a screen has to be found in it.
        if page.iter().all(|entry| entry.entries().is_none()) {
            return page;
        }
        page.iter()
            .find(|entry| entry.title() == name)
            .unwrap_or_else(|| panic!("{name} is not in the {title} screen list"))
            .entries()
            .expect("a screen opens its values")
            .to_vec()
    }

    /// The same page the HDR one is, twice over: the screen is named before
    /// anything is offered, because what is offered is one screen's own.
    #[test]
    fn the_mode_pages_name_the_screen_before_they_offer_a_value() {
        with_modes(
            &[
                (
                    FIRST,
                    &[(3840, 2160, 120), (3840, 2160, 60), (1920, 1080, 60)],
                ),
                (SECOND, &[(1920, 1080, 144), (1920, 1080, 60)]),
                (AWKWARD, &[(2560, 1440, 60), (1280, 720, 60)]),
            ],
            || {
                let column = column();
                let display = column[1].entries().expect("Display opens a column");
                assert_eq!(
                    display.iter().map(Entry::title).collect::<Vec<_>>(),
                    [
                        "Resolution",
                        "Refresh rate",
                        "Orientation",
                        "Display order",
                        "Night light",
                        "HDR",
                        "OLED protection"
                    ],
                    "the shape of the picture comes before what it carries"
                );

                // Every screen the compositor reports modes for, on both
                // pages, in the order it announced them. A screen with one
                // size is still a screen that has a size, and the page says
                // which — hiding it would leave the user asking where their
                // second monitor went.
                for title in ["Resolution", "Refresh rate"] {
                    assert_eq!(
                        page(title).iter().map(Entry::title).collect::<Vec<_>>(),
                        [FIRST, SECOND, AWKWARD],
                        "{title} lists every screen"
                    );
                }

                // Each screen row says what it is showing, so the page answers
                // before it is stepped into — each about its own half, and the
                // rate row naming the size whose rates are underneath it.
                assert_eq!(page("Resolution")[0].comment(), Some("3840 × 2160"));
                assert_eq!(
                    page("Refresh rate")[0].comment(),
                    Some("120 Hz at 3840 × 2160")
                );

                for title in ["Resolution", "Refresh rate"] {
                    for screen in page(title) {
                        let values = screen.entries().expect("a screen opens its values");
                        assert_eq!(
                            values.iter().filter(|entry| entry.chosen()).count(),
                            1,
                            "one value is in force under {title} on {}",
                            screen.title()
                        );
                        assert!(values.iter().all(|entry| entry.setting().is_some()));
                    }
                }

                // A size is listed once however many rates it carries, and
                // says what the best of them is.
                let sizes = values_for("Resolution", FIRST);
                assert_eq!(
                    sizes.iter().map(Entry::title).collect::<Vec<_>>(),
                    ["3840 × 2160", "1920 × 1080"]
                );
                assert_eq!(
                    sizes[0].comment(),
                    Some("Up to 120 Hz, and what this display asks for")
                );
                assert_eq!(sizes[1].comment(), Some("Up to 60 Hz"));

                // And the rates of the size it is set to, fastest first. The
                // screen at 4K is offered 4K's two rates and not the third,
                // which belongs to a size it is not on.
                assert_eq!(
                    values_for("Refresh rate", FIRST)
                        .iter()
                        .map(Entry::title)
                        .collect::<Vec<_>>(),
                    ["120 Hz", "60 Hz"]
                );

                let rates = values_for("Refresh rate", SECOND);
                assert_eq!(
                    rates.iter().map(Entry::title).collect::<Vec<_>>(),
                    ["144 Hz", "60 Hz"]
                );
                assert_eq!(rates[0].comment(), Some("What this display asks for"));
                assert_eq!(rates[1].comment(), None, "a rate is its own label");
            },
        );
    }

    /// One screen is not a choice of screen, so its values stand where the
    /// list would have — and a page with no screen behind it at all says so
    /// rather than opening onto an empty column.
    #[test]
    fn one_screen_is_not_something_to_choose_between_either() {
        with_modes(&[(FIRST, &[(1920, 1080, 60), (1280, 720, 75)])], || {
            let resolution = display_row("Resolution");
            assert_eq!(
                resolution.comment(),
                Some(format!("{FIRST} — 1920 × 1080").as_str()),
                "the one screen is named where the list would have been"
            );
            assert_eq!(
                page("Resolution")
                    .iter()
                    .map(Entry::title)
                    .collect::<Vec<_>>(),
                ["1920 × 1080", "1280 × 720"],
                "the sizes stand where the screen list would have"
            );
            // And they are that screen's sizes, not a nameless set.
            assert_eq!(
                page("Resolution")[1].setting(),
                Some(setting(
                    intern(FIRST),
                    DisplayValue::Resolution(Resolution {
                        width: 1280,
                        height: 720,
                    })
                ))
            );

            // The rate page is the size page's child: 75 Hz belongs to a size
            // this screen is not set to, so it is not among the rates offered
            // for the size it is.
            assert_eq!(
                display_row("Refresh rate").comment(),
                Some(format!("{FIRST} — 60 Hz at 1920 × 1080").as_str())
            );
            assert_eq!(
                page("Refresh rate")
                    .iter()
                    .map(Entry::title)
                    .collect::<Vec<_>>(),
                ["60 Hz"]
            );

            // Choose the other size and the rate page follows it, which is the
            // only way 75 Hz is reachable — and the only way it means anything.
            assert!(apply_with(
                setting(
                    intern(FIRST),
                    DisplayValue::Resolution(Resolution {
                        width: 1280,
                        height: 720,
                    })
                ),
                |_| {}
            ));
            assert_eq!(
                page("Refresh rate")
                    .iter()
                    .map(Entry::title)
                    .collect::<Vec<_>>(),
                ["75 Hz"]
            );
        });

        // A second screen brings the choice of screen back, whatever either of
        // them has to offer.
        with_modes(
            &[
                (FIRST, &[(1920, 1080, 60), (1280, 720, 60)]),
                (SECOND, &[(1920, 1080, 60)]),
            ],
            || {
                for title in ["Resolution", "Refresh rate"] {
                    assert_eq!(
                        page(title).iter().map(Entry::title).collect::<Vec<_>>(),
                        [FIRST, SECOND],
                        "{title} keeps the screen with one mode"
                    );
                }
                // Including the screen that has only the one, which is still
                // worth a row: it says what that screen is showing.
                assert_eq!(
                    values_for("Resolution", SECOND)
                        .iter()
                        .map(Entry::title)
                        .collect::<Vec<_>>(),
                    ["1920 × 1080"]
                );
            },
        );

        // Nothing reports a mode: a nested session, whose window size belongs
        // to the compositor outside. Both rows say so.
        for reported in [&[(AWKWARD, &[][..])][..], &[]] {
            with_modes(reported, || {
                for title in ["Resolution", "Refresh rate"] {
                    let page = page(title);
                    assert_eq!(page.len(), 1, "never empty, or the bar cannot enter it");
                    assert_eq!(page[0].title(), "No display reports its modes");
                    assert_eq!(page[0].setting(), None, "and it is a reading");
                    assert!(page[0].entries().is_none(), "a dead end, not a column");
                }
            });
        }
    }

    /// The rate page offers the rates of the size in force, and every one of
    /// them — a rate belongs to a mode, and one listed under a size that cannot
    /// be given it is a row that would have to move the size to keep its word.
    #[test]
    fn the_rates_are_the_ones_the_size_in_force_carries() {
        let all = &[
            (3840, 2160, 60),
            (1920, 1080, 144),
            (1920, 1080, 120),
            (1920, 1080, 60),
        ];
        with_modes(&[(FIRST, all)], || {
            let rates = || -> Vec<String> {
                page("Refresh rate")
                    .iter()
                    .map(|entry| entry.title().to_string())
                    .collect()
            };

            // Showing 4K, which carries one rate of the four. The three that
            // 1080p carries are 1080p's, and are not on this page.
            assert_eq!(rates(), ["60 Hz"], "4K has the one rate");
            assert_eq!(
                page("Refresh rate")[0].comment(),
                Some("What this display asks for")
            );
            assert!(page("Refresh rate")[0].chosen(), "60 Hz is what it runs");

            // Set the size, and the rate page is that size's — before the
            // compositor has confirmed anything, because the page follows the
            // choice that was made rather than the display catching up to it.
            assert!(apply_with(
                setting(
                    intern(FIRST),
                    DisplayValue::Resolution(Resolution {
                        width: 1920,
                        height: 1080,
                    })
                ),
                |_| {}
            ));
            assert_eq!(rates(), ["144 Hz", "120 Hz", "60 Hz"], "all of 1080p's");
            assert_eq!(
                display_row("Refresh rate").comment(),
                Some(format!("{FIRST} — 3 rates at 1920 × 1080").as_str()),
                "and says so, having nothing running at that size to name"
            );

            // Choosing one sets the rate and leaves the size where it was.
            assert!(apply_with(
                setting(intern(FIRST), DisplayValue::RefreshRate(144_000)),
                |_| {}
            ));
            assert_eq!(mode_for(FIRST), Some(mode(1920, 1080, 144)));
        });
    }

    /// Every row sets what it is named after, on the screen it was chosen on —
    /// and the file says so, for that screen and no other.
    #[test]
    fn choosing_a_size_or_a_rate_sets_it() {
        // 1080p at two rates, and a smaller size with a rate of its own — so
        // there is a rate on this display that the size in force has not got,
        // and no row may reach it.
        let offered: &[(u32, u32, u32)] = &[(1920, 1080, 144), (1920, 1080, 60), (1280, 720, 75)];
        with_modes(
            &[
                (FIRST, offered),
                (SECOND, &[(2560, 1440, 60), (1920, 1080, 60)]),
            ],
            || {
                // A rate sets the rate and nothing else: the size it was listed
                // under is the size it keeps. Taken before any size is chosen,
                // so the page is about what the display is actually showing.
                let all = listed(offered);
                let under = size_shown(FIRST, &all).expect("the page is about some size");
                for entry in values_for("Refresh rate", FIRST) {
                    let Some(chosen @ Setting::Display { value, .. }) = entry.setting() else {
                        panic!("{} sets nothing", entry.title());
                    };
                    let DisplayValue::RefreshRate(refresh) = value else {
                        panic!("a rate row sets {value:?}");
                    };
                    assert_eq!(entry.title(), hertz(refresh).unwrap());
                    assert!(apply_with(chosen, |_| {}));

                    let live = mode_for(FIRST).unwrap();
                    assert_eq!(live.refresh, refresh, "the rate is what was chosen");
                    assert_eq!(live.resolution, under, "and the size is left alone");
                    // Which together are a mode this display actually lists,
                    // never a combination invented from two halves that do not
                    // go together.
                    assert!(
                        all.iter().any(|offered| offered.mode == live),
                        "{live:?} is not a mode this display offers"
                    );
                }

                for entry in values_for("Resolution", FIRST) {
                    let Some(chosen @ Setting::Display { display, value }) = entry.setting() else {
                        panic!("{} sets nothing", entry.title());
                    };
                    assert_eq!(display, FIRST);
                    let DisplayValue::Resolution(resolution) = value else {
                        panic!("a size row sets {value:?}");
                    };
                    // The row says what it sets.
                    assert_eq!(entry.title(), pixels(resolution));

                    let mut written = None;
                    assert!(apply_with(chosen, |stored| written = Some(stored.display.clone())));
                    assert_eq!(
                        mode_for(FIRST).map(|mode| mode.resolution),
                        Some(resolution)
                    );
                    assert_eq!(mode_for(SECOND), None, "the other screen is untouched");

                    let written = written.expect("every setting is written down");
                    assert_eq!(
                        written[FIRST].mode.as_deref(),
                        Some(mode_for(FIRST).unwrap().to_config().as_str())
                    );
                    assert!(!written.contains_key(SECOND));
                    // A mode is not a colour pipeline: choosing one must not
                    // invent HDR settings for that screen.
                    assert_eq!(written[FIRST].hdr, None);
                }
            },
        );
    }

    /// A size takes its rate with it where the new size has that rate, and
    /// falls back to the fastest where it does not — rather than asking for a
    /// mode the display has never listed.
    #[test]
    fn a_size_carries_the_rate_where_it_can() {
        with_modes(
            &[(
                FIRST,
                &[
                    (1920, 1080, 60),
                    (1920, 1080, 120),
                    (2560, 1440, 120),
                    (3840, 2160, 30),
                ],
            )],
            || {
                let choose = |value: DisplayValue| {
                    assert!(apply_with(setting(intern(FIRST), value), |_| {}));
                };

                // Running 1080p60. Up to 1440p, which offers no 60: the rate
                // cannot come along, so the fastest of that size is asked for.
                choose(DisplayValue::Resolution(Resolution {
                    width: 2560,
                    height: 1440,
                }));
                assert_eq!(mode_for(FIRST), Some(mode(2560, 1440, 0)));

                // Now with a rate chosen, which fills in the size it was
                // listed under and leaves it there.
                choose(DisplayValue::RefreshRate(120_000));
                assert_eq!(mode_for(FIRST), Some(mode(2560, 1440, 120)));

                // And a size that has that rate takes it along.
                choose(DisplayValue::Resolution(Resolution {
                    width: 1920,
                    height: 1080,
                }));
                assert_eq!(
                    mode_for(FIRST),
                    Some(mode(1920, 1080, 120)),
                    "1080p has 120 Hz, so it carries over"
                );

                // And a size that does not have it drops back to its fastest.
                choose(DisplayValue::Resolution(Resolution {
                    width: 3840,
                    height: 2160,
                }));
                assert_eq!(mode_for(FIRST), Some(mode(3840, 2160, 0)));
            },
        );
    }

    /// A rate chosen on a display nobody has set a size for is still a whole
    /// mode: the size comes from what that display is running.
    #[test]
    fn a_rate_alone_is_asked_for_at_the_size_being_shown() {
        with_modes(&[(FIRST, &[(1920, 1080, 60), (1920, 1080, 144)])], || {
            assert_eq!(mode_for(FIRST), None, "nothing has been chosen yet");
            assert!(apply_with(
                setting(intern(FIRST), DisplayValue::RefreshRate(144_000)),
                |_| {}
            ));
            assert_eq!(mode_for(FIRST), Some(mode(1920, 1080, 144)));
        });

        // With nothing known about the display at all there is no mode to ask
        // for, and saying so beats asking for half of one.
        with_modes(&[], || {
            assert!(!apply_with(
                setting(intern(FIRST), DisplayValue::RefreshRate(144_000)),
                |_| panic!("nothing may be written")
            ));
            assert_eq!(mode_for(FIRST), None);
        });
    }

    /// The mark is on what the display is *running*, not on what was last
    /// asked for. A mode the hardware refused must not read as chosen.
    #[test]
    fn the_mark_follows_the_display_rather_than_the_request() {
        with_modes(&[(FIRST, &[(1920, 1080, 60), (1280, 720, 60)])], || {
            let marked = || -> String {
                page("Resolution")
                    .iter()
                    .find(|entry| entry.chosen())
                    .expect("one size is in force")
                    .title()
                    .to_string()
            };
            assert_eq!(marked(), "1920 × 1080");

            let smaller = Resolution {
                width: 1280,
                height: 720,
            };
            assert!(apply_with(
                setting(intern(FIRST), DisplayValue::Resolution(smaller)),
                |_| {}
            ));
            assert_eq!(mode_for(FIRST).map(|mode| mode.resolution), Some(smaller));
            assert_eq!(marked(), "1920 × 1080", "the display is still on it");

            // The compositor says it took, and only now does the mark move.
            note_modes(vec![(
                FIRST.to_string(),
                listed(&[(1280, 720, 60), (1920, 1080, 60)]),
            )]);
            assert_eq!(marked(), "1280 × 720");
        });
    }

    /// A list of sizes descends, whatever order the driver listed them in: a
    /// page nobody can scan is a page nobody can choose from.
    #[test]
    fn the_sizes_are_listed_largest_first() {
        with_modes(
            &[(
                FIRST,
                &[
                    (1280, 720, 60),
                    (3840, 2160, 60),
                    (1920, 1080, 60),
                    (1920, 1080, 144),
                ],
            )],
            || {
                assert_eq!(
                    page("Resolution")
                        .iter()
                        .map(Entry::title)
                        .collect::<Vec<_>>(),
                    ["3840 × 2160", "1920 × 1080", "1280 × 720"],
                    "each size once, largest first"
                );
            },
        );
    }

    /// A mode is written in the format the compositor's own config uses, and
    /// read back as what it was.
    #[test]
    fn a_mode_survives_the_file() {
        let written = [
            (mode(2560, 1440, 144), "2560x1440@144"),
            // A rate that is not a whole number of hertz, which most of them
            // are not: 59.94 must not come back as 59 or as 60.
            (
                Mode {
                    resolution: Resolution {
                        width: 1920,
                        height: 1080,
                    },
                    refresh: 59_940,
                },
                "1920x1080@59.94",
            ),
            // And a size chosen with no rate to carry over, which asks the
            // compositor for the fastest of that size.
            (mode(1024, 768, 0), "1024x768"),
        ];
        for (mode, written) in written {
            assert_eq!(mode.to_config(), written);
            assert_eq!(Mode::from_config(written), Some(mode));
        }

        // A hand-edited line that is not a mode is not one.
        for nonsense in ["", "1920", "1920x", "wide x tall", "1920x1080@fast"] {
            assert_eq!(Mode::from_config(nonsense), None, "{nonsense:?}");
        }
    }

    /// A file whose mode line is nonsense still hands over the rest of itself:
    /// the accent and the HDR settings are not the typo's to take.
    #[test]
    fn a_mode_that_is_not_a_mode_is_dropped_on_its_own() {
        let file = "\
[display.TEST-OUT-1]
mode = \"as big as it goes\"
hdr = true
";
        with_modes(&[], || {
            adopt(toml::from_str(file).unwrap());
            assert_eq!(mode_for(FIRST), None);
            assert!(hdr_for(FIRST).enabled, "the rest of the section stands");
        });
    }

    /// A screen given only a mode is not thereby given HDR settings, and one
    /// given only HDR settings keeps them: the file is the union of the two,
    /// not either one of them.
    #[test]
    fn the_file_holds_both_kinds_of_setting_per_screen() {
        with_modes(&[(FIRST, &[(1920, 1080, 60), (1280, 720, 60)])], || {
            let mut written = None;
            assert!(apply_with(
                setting(
                    intern(FIRST),
                    DisplayValue::Resolution(Resolution {
                        width: 1280,
                        height: 720,
                    })
                ),
                |_| {}
            ));
            assert!(apply_with(
                setting(intern(SECOND), DisplayValue::Hdr(true)),
                |stored| written = Some(stored.display.clone())
            ));

            let written = written.unwrap();
            assert_eq!(written[FIRST].mode.as_deref(), Some("1280x720@60"));
            assert_eq!(written[FIRST].hdr, None);
            assert_eq!(written[SECOND].mode, None);
            assert_eq!(written[SECOND].hdr, Some(true));

            // And a round trip through the file leaves both where they were.
            let body = toml::to_string_pretty(&stored()).unwrap();
            adopt(toml::from_str(&body).unwrap());
            assert_eq!(mode_for(FIRST), Some(mode(1280, 720, 60)));
            assert!(hdr_for(SECOND).enabled);
            assert_eq!(mode_for(SECOND), None);
        });
    }

    /// Highlighting a size or a rate must not set one: a mode change blanks
    /// the display for a second, and walking a list of them would be unusable.
    #[test]
    fn walking_over_a_mode_changes_nothing() {
        with_modes(&[(FIRST, &[(1920, 1080, 60), (1280, 720, 60)])], || {
            for title in ["Resolution", "Refresh rate"] {
                for entry in page(title) {
                    preview(entry.setting());
                }
            }
            assert_eq!(mode_for(FIRST), None);
        });
    }

    /// Two modes that round to the same rate are both kept, and both say which
    /// they are.
    ///
    /// A high-refresh panel is the ordinary case rather than a curiosity: the
    /// timing at its top size and the one at a smaller size are rarely the same
    /// number of hertz to the thousandth, and rounding them to something
    /// readable lands both on "120 Hz". Dropping either would be dropping a
    /// mode the display offers; printing both as "120 Hz" would be two rows
    /// nobody can tell apart.
    #[test]
    fn rates_that_round_alike_are_told_apart() {
        // Whole hertz where nothing clashes, thousandths where something does.
        let apart = rate_titles(&[(239_970, true), (143_991, false), (60_000, false)]);
        assert_eq!(apart, ["239.97 Hz", "143.99 Hz", "60 Hz"]);

        let clashing = rate_titles(&[(120_000, false), (119_998, false), (59_940, false)]);
        assert_eq!(clashing, ["120.000 Hz", "119.998 Hz", "59.94 Hz"]);

        // And on the page itself, where one size carries both.
        with_modes(&[(FIRST, &[(2560, 1440, 0)])], || {
            // Those rates are not whole numbers of hertz, which the tuple form
            // cannot express, so they are noted through the list the page is
            // built from instead.
            let panel = Resolution {
                width: 2560,
                height: 1440,
            };
            note_modes(vec![(
                FIRST.to_string(),
                [(119_998, true), (120_000, false)]
                    .into_iter()
                    .map(|(refresh, own)| Offered {
                        mode: Mode {
                            resolution: panel,
                            refresh,
                        },
                        current: own,
                        preferred: own,
                    })
                    .collect(),
            )]);
            let rates = page("Refresh rate");
            assert_eq!(
                rates.iter().map(Entry::title).collect::<Vec<_>>(),
                ["120.000 Hz", "119.998 Hz"],
                "both are the display's, and neither is the other"
            );
            assert_eq!(rates[0].comment(), None, "a rate is its own label");
            assert!(rates[1].chosen(), "and the one on screen is marked");
        });
    }

    // -----------------------------------------------------------------------
    // orientation
    // -----------------------------------------------------------------------

    /// The third page of the same shape: several screens are named first, and
    /// every one of them is offered all four turns — unlike a mode, none of
    /// which is a list a display has.
    #[test]
    fn the_orientation_page_names_the_screen_before_it_offers_a_turn() {
        with_turns(
            &[
                (FIRST, Orientation::Landscape),
                (SECOND, Orientation::Portrait),
                (AWKWARD, Orientation::LandscapeFlipped),
            ],
            || {
                assert_eq!(
                    page("Orientation")
                        .iter()
                        .map(Entry::title)
                        .collect::<Vec<_>>(),
                    [FIRST, SECOND, AWKWARD],
                    "every screen the compositor turns is listed"
                );
                // Each screen row says which way up it is, so the page answers
                // before it is stepped into.
                assert_eq!(page("Orientation")[1].comment(), Some("90° Rotation"));

                for screen in page("Orientation") {
                    let turns = screen.entries().expect("a screen opens its turns");
                    assert_eq!(
                        turns.iter().map(Entry::title).collect::<Vec<_>>(),
                        [
                            "0° Rotation",
                            "90° Rotation",
                            "180° Rotation",
                            "270° Rotation",
                        ],
                        "the same four on every screen"
                    );
                    // And each is drawn as the shape it stands for: four
                    // distinct monitors, none of them the bead every other
                    // value in this tree wears.
                    let drawn: Vec<Option<&str>> = turns.iter().map(Entry::icon).collect();
                    assert_eq!(
                        drawn,
                        [
                            Some(icons::SETTING_ROTATION_0),
                            Some(icons::SETTING_ROTATION_90),
                            Some(icons::SETTING_ROTATION_180),
                            Some(icons::SETTING_ROTATION_270),
                        ],
                        "a turn is drawn, not beaded"
                    );
                    assert_eq!(
                        turns.iter().filter(|entry| entry.chosen()).count(),
                        1,
                        "one turn is in force on {}",
                        screen.title()
                    );
                    assert!(turns.iter().all(|entry| entry.setting().is_some()));
                }

                // And they are that screen's turns, not a nameless set.
                assert_eq!(
                    values_for("Orientation", SECOND)[3].setting(),
                    Some(setting(
                        intern(SECOND),
                        DisplayValue::Orientation(Orientation::PortraitFlipped)
                    ))
                );
            },
        );
    }

    /// One screen is not a choice of screen here either, and a session where
    /// nothing can be turned says so rather than opening onto an empty column.
    #[test]
    fn one_screen_is_not_something_to_choose_between_to_turn() {
        with_turns(&[(FIRST, Orientation::Portrait)], || {
            assert_eq!(
                display_row("Orientation").comment(),
                Some(format!("{FIRST} — 90° Rotation").as_str()),
                "the one screen is named where the list would have been"
            );
            let turns = page("Orientation");
            assert_eq!(
                turns.len(),
                4,
                "the turns stand where the screen list would"
            );
            assert!(turns[1].chosen(), "and the one on screen is marked");
            assert_eq!(
                turns[1].setting(),
                Some(setting(
                    intern(FIRST),
                    DisplayValue::Orientation(Orientation::Portrait)
                ))
            );
        });

        with_turns(&[], || {
            let empty = page("Orientation");
            assert_eq!(empty.len(), 1);
            assert_eq!(empty[0].title(), "No display can be turned");
            assert_eq!(empty[0].setting(), None, "a reason is not a choice");
            assert_eq!(empty[0].icon(), Some(icons::SETTING_INFO));
        });
    }

    /// The mark is on what the compositor says it is drawing, not on what was
    /// last asked for — and a screen the config has put into a mirrored
    /// orientation is in none of the four, which the page says by marking none
    /// of them and naming what it is in instead.
    #[test]
    fn the_turn_marked_is_the_one_the_display_is_drawn_at() {
        with_turns(&[(FIRST, Orientation::Landscape)], || {
            assert!(apply_with(
                setting(
                    intern(FIRST),
                    DisplayValue::Orientation(Orientation::Portrait)
                ),
                |_| {}
            ));
            assert_eq!(turn_for(FIRST), Some(Orientation::Portrait));
            assert!(
                page("Orientation")[0].chosen(),
                "the compositor has not said it turned, so the page has not moved"
            );

            // The compositor answering is what moves the mark.
            note_turned(vec![(FIRST.to_string(), Orientation::Portrait)]);
            assert!(page("Orientation")[1].chosen());
        });

        with_turns(&[(FIRST, Orientation::MirroredFlipped)], || {
            assert_eq!(
                display_row("Orientation").comment(),
                Some(format!("{FIRST} — Mirrored, 180° rotation").as_str())
            );
            assert!(
                page("Orientation").iter().all(|entry| !entry.chosen()),
                "it is in none of the four, and nothing may say otherwise"
            );
        });
    }

    /// Highlighting a turn must not make it: every display on the page
    /// re-tiles everything on it, which is the reason no Display value
    /// previews.
    #[test]
    fn walking_over_a_turn_changes_nothing() {
        with_turns(&[(FIRST, Orientation::Landscape)], || {
            for entry in page("Orientation") {
                preview(entry.setting());
            }
            assert_eq!(turn_for(FIRST), None);
        });
    }

    /// A turn survives the file, in the compositor's own spelling, and is
    /// filed on its own: turning a screen must not write it a mode or a colour
    /// pipeline nobody asked for.
    #[test]
    fn a_turn_survives_the_file() {
        with_turns(&[(FIRST, Orientation::Landscape)], || {
            let mut written = None;
            assert!(apply_with(
                setting(
                    intern(FIRST),
                    DisplayValue::Orientation(Orientation::PortraitFlipped)
                ),
                |stored| written = Some(stored.display.clone())
            ));

            let written = written.unwrap();
            assert_eq!(written[FIRST].transform.as_deref(), Some("270"));
            assert_eq!(written[FIRST].mode, None);
            assert_eq!(written[FIRST].hdr, None);

            let body = toml::to_string_pretty(&stored()).unwrap();
            adopt(toml::from_str(&body).unwrap());
            assert_eq!(turn_for(FIRST), Some(Orientation::PortraitFlipped));
        });
    }

    /// Every spelling the compositor's own config accepts is accepted here,
    /// and anything else is dropped with a word about it rather than taking
    /// the rest of the display's section down with it.
    #[test]
    fn an_orientation_that_is_not_one_is_dropped_on_its_own() {
        for (raw, turn) in [
            ("normal", Orientation::Landscape),
            ("0", Orientation::Landscape),
            ("90", Orientation::Portrait),
            ("180", Orientation::LandscapeFlipped),
            ("270", Orientation::PortraitFlipped),
            ("flipped", Orientation::Mirrored),
            ("flipped-90", Orientation::MirroredPortrait),
            ("flipped180", Orientation::MirroredFlipped),
            (" FLIPPED-270 ", Orientation::MirroredPortraitFlipped),
        ] {
            assert_eq!(Orientation::from_key(raw), Some(turn), "{raw}");
            // And every one of them is written back the way this shell spells
            // it, which is a spelling the compositor reads.
            assert_eq!(Orientation::from_key(turn.key()), Some(turn));
        }
        assert_eq!(Orientation::from_key("sideways"), None);

        let file = "\
[display.TEST-OUT-1]
transform = \"sideways\"
hdr = true
";
        with_turns(&[], || {
            adopt(toml::from_str(file).unwrap());
            assert_eq!(turn_for(FIRST), None);
            assert!(hdr_for(FIRST).enabled, "the rest of the section stands");
        });
    }

    /// The numbers on the wire are `wl_output`'s, in both directions: a shell
    /// that renumbered them would turn every display the wrong way.
    #[test]
    fn the_turns_are_counted_as_wl_output_counts_them() {
        for (code, turn) in [
            (0, Orientation::Landscape),
            (1, Orientation::Portrait),
            (2, Orientation::LandscapeFlipped),
            (3, Orientation::PortraitFlipped),
            (4, Orientation::Mirrored),
            (5, Orientation::MirroredPortrait),
            (6, Orientation::MirroredFlipped),
            (7, Orientation::MirroredPortraitFlipped),
        ] {
            assert_eq!(Orientation::from_code(code), Some(turn));
            assert_eq!(turn.code(), code);
        }
        // A ninth value is a compositor speaking a later protocol, and is not
        // guessed at.
        assert_eq!(Orientation::from_code(8), None);
    }

    // -----------------------------------------------------------------------
    // display order
    // -----------------------------------------------------------------------

    /// The Display order page's own shape: every screen the compositor
    /// arranges, in the order it arranges them, each saying where it stands and
    /// opening onto the places it could stand in instead.
    #[test]
    fn the_order_page_names_every_screen_and_where_it_stands() {
        with_places(&[FIRST, SECOND, AWKWARD], || {
            assert_eq!(
                display_row("Display order").icon(),
                Some(icons::SETTING_ORDER),
                "the one page here about the screens rather than a screen"
            );
            assert_eq!(
                page("Display order")
                    .iter()
                    .map(Entry::title)
                    .collect::<Vec<_>>(),
                [FIRST, SECOND, AWKWARD],
                "the screens are listed in the order they are laid out in"
            );
            // Each screen row says which place it holds, so the page answers
            // before it is stepped into — and says it the way the user counts,
            // from one.
            assert_eq!(
                page("Display order")
                    .iter()
                    .map(|entry| entry.comment().unwrap_or_default().to_string())
                    .collect::<Vec<_>>(),
                ["Display 1", "Display 2", "Display 3"]
            );

            for screen in page("Display order") {
                let places = screen.entries().expect("a screen opens its places");
                assert_eq!(
                    places.iter().map(Entry::title).collect::<Vec<_>>(),
                    ["Display 1", "Display 2", "Display 3"],
                    "as many places as there are screens, on every screen"
                );
                assert_eq!(
                    places.iter().filter(|entry| entry.chosen()).count(),
                    1,
                    "one place is in force on {}",
                    screen.title()
                );
                assert!(places.iter().all(|entry| entry.setting().is_some()));
            }

            // Every row that is not the one it is standing on says who it
            // would be trading with, because that is what pressing it does.
            let places = values_for("Display order", SECOND);
            assert_eq!(
                places[0].comment(),
                Some(format!("Trades places with {FIRST}").as_str())
            );
            assert_eq!(places[1].comment(), Some("Where this screen is now"));
            assert_eq!(
                places[2].comment(),
                Some(format!("Trades places with {AWKWARD}").as_str())
            );

            // And they are that screen's places, not a nameless set. Counted
            // from zero on the wire, whatever the row is called.
            assert_eq!(
                places[0].setting(),
                Some(setting(intern(SECOND), DisplayValue::Place(0)))
            );
        });
    }

    /// One screen is the whole of its own arrangement, and a session that
    /// arranges nothing says so — neither opens onto a list of places, because
    /// neither has one.
    #[test]
    fn one_screen_is_not_an_order() {
        with_places(&[FIRST], || {
            assert_eq!(
                display_row("Display order").comment(),
                Some(format!("{FIRST} — the only screen").as_str())
            );
            let alone = page("Display order");
            assert_eq!(alone.len(), 1);
            assert_eq!(alone[0].title(), "Only one display");
            assert_eq!(alone[0].setting(), None, "a reason is not a choice");
            assert_eq!(alone[0].icon(), Some(icons::SETTING_INFO));
        });

        with_places(&[], || {
            let empty = page("Display order");
            assert_eq!(empty.len(), 1);
            assert_eq!(empty[0].title(), "No display can be moved");
            assert_eq!(empty[0].setting(), None);
            assert_eq!(empty[0].icon(), Some(icons::SETTING_INFO));
        });
    }

    /// Choosing a place trades two screens, and writes down the whole order
    /// rather than the screen that was pressed: half an arrangement is one no
    /// two screens agree on.
    ///
    /// The mark stays where the compositor last put it until the compositor
    /// says otherwise, which is the rule every page under Display follows.
    #[test]
    fn choosing_a_place_trades_two_screens() {
        with_places(&[FIRST, SECOND, AWKWARD], || {
            // The third screen is asked to become the first one.
            assert!(apply_with(
                setting(intern(AWKWARD), DisplayValue::Place(0)),
                |_| {}
            ));

            assert_eq!(
                place_for(AWKWARD),
                Some(0),
                "it takes the place it was given"
            );
            assert_eq!(place_for(FIRST), Some(2), "and the screen there takes its");
            assert_eq!(place_for(SECOND), Some(1), "the screen between them stays");

            assert_eq!(
                wanted_order(),
                [AWKWARD, SECOND, FIRST],
                "which is the order the compositor is asked for"
            );
            assert!(
                values_for("Display order", AWKWARD)[2].chosen(),
                "the compositor has not said it moved, so the mark has not"
            );

            // The compositor answering is what moves the mark — and only the
            // mark: the rows are the screens, listed as they were announced,
            // and they hold still while the arrangement changes under them.
            note_places(vec![
                (FIRST.to_string(), 2),
                (SECOND.to_string(), 1),
                (AWKWARD.to_string(), 0),
            ]);
            assert_eq!(
                page("Display order")
                    .iter()
                    .map(Entry::title)
                    .collect::<Vec<_>>(),
                [FIRST, SECOND, AWKWARD],
                "the same rows, in the same places on the page"
            );
            assert!(values_for("Display order", AWKWARD)[0].chosen());
            assert!(values_for("Display order", FIRST)[2].chosen());
            // And each row says where its screen now stands.
            assert_eq!(page("Display order")[0].comment(), Some("Display 3"));
            assert_eq!(page("Display order")[2].comment(), Some("Display 1"));
            // The screen that traded is named on the row that would trade back.
            assert_eq!(
                values_for("Display order", AWKWARD)[2].comment(),
                Some(format!("Trades places with {FIRST}").as_str())
            );
        });
    }

    /// A place chosen on a screen that is no longer in the arrangement changes
    /// nothing: the row was drawn before the display went away.
    #[test]
    fn a_place_on_a_screen_that_is_gone_is_refused() {
        with_places(&[FIRST, SECOND], || {
            assert!(!apply_with(
                setting(intern(AWKWARD), DisplayValue::Place(0)),
                |_| panic!("nothing may be written for a screen that is not there")
            ));
            assert_eq!(place_for(FIRST), None);

            // And so is a place past the end of the list, which is the same
            // thing one step later: an arrangement this session does not have.
            assert!(!apply_with(
                setting(intern(FIRST), DisplayValue::Place(2)),
                |_| panic!("there is no third place on a two-screen desk")
            ));
        });
    }

    /// Highlighting a place must not take it. Every screen on the desk moves,
    /// and every window on them is re-tiled — which is the reason no Display
    /// value previews.
    #[test]
    fn walking_over_a_place_changes_nothing() {
        with_places(&[FIRST, SECOND], || {
            for screen in page("Display order") {
                for entry in screen.entries().expect("a screen opens its places") {
                    preview(entry.setting());
                }
            }
            assert_eq!(place_for(FIRST), None);
            assert_eq!(place_for(SECOND), None);
        });
    }

    /// The order survives the file, counted from one there and from zero here,
    /// and is filed on its own: moving a screen must not write it a mode, a
    /// turn or a colour pipeline nobody asked for.
    #[test]
    fn an_order_survives_the_file() {
        with_places(&[FIRST, SECOND], || {
            let mut written = None;
            assert!(apply_with(
                setting(intern(SECOND), DisplayValue::Place(0)),
                |stored| written = Some(stored.display.clone())
            ));

            let written = written.unwrap();
            assert_eq!(written[SECOND].order, Some(1), "the file counts from one");
            assert_eq!(written[FIRST].order, Some(2));
            assert_eq!(written[FIRST].mode, None);
            assert_eq!(written[FIRST].transform, None);
            assert_eq!(written[FIRST].hdr, None);

            let body = toml::to_string_pretty(&stored()).unwrap();
            adopt(toml::from_str(&body).unwrap());
            assert_eq!(place_for(SECOND), Some(0));
            assert_eq!(place_for(FIRST), Some(1));
        });
    }

    /// A place before the first one is not a place. It is dropped with a word
    /// about it rather than read as the first, because a file naming both `0`
    /// and `1` would otherwise be an order with two first screens.
    #[test]
    fn a_place_before_the_first_one_is_dropped() {
        with_places(&[FIRST, SECOND], || {
            let mut stored = Stored::default();
            stored.display.insert(
                FIRST.to_string(),
                StoredDisplay {
                    order: Some(0),
                    ..StoredDisplay::default()
                },
            );
            stored.display.insert(
                SECOND.to_string(),
                StoredDisplay {
                    order: Some(1),
                    ..StoredDisplay::default()
                },
            );
            adopt(stored);

            assert_eq!(place_for(FIRST), None);
            assert_eq!(place_for(SECOND), Some(0));
        });
    }

    /// What the shell asks for, given what it remembers and what is plugged in
    /// now: the remembered order, and a screen it has never heard of left where
    /// the compositor put it, which is at the back.
    #[test]
    fn a_screen_the_file_has_never_seen_keeps_its_place_at_the_back() {
        with_places(&[FIRST, SECOND, AWKWARD], || {
            // An order remembered for two of the three.
            PLACE.lock().unwrap().insert(SECOND.to_string(), 0);
            PLACE.lock().unwrap().insert(FIRST.to_string(), 1);

            assert_eq!(wanted_order(), [SECOND, FIRST, AWKWARD]);
        });

        // And the remembered order is kept where only some of it is plugged
        // in: the screen that is missing takes nobody's place with it.
        with_places(&[AWKWARD, FIRST], || {
            PLACE.lock().unwrap().insert(FIRST.to_string(), 0);
            PLACE.lock().unwrap().insert(SECOND.to_string(), 1);
            PLACE.lock().unwrap().insert(AWKWARD.to_string(), 2);

            assert_eq!(wanted_order(), [FIRST, AWKWARD]);
        });
    }

    /// The arrangement is read out of the places the screens report, whatever
    /// order the screens themselves are listed in — and the list keeps the
    /// order they were announced in, which is what holds the rows still.
    #[test]
    fn the_arrangement_is_read_from_the_places_rather_than_the_listing() {
        with_places(&[], || {
            assert!(note_places(vec![
                (AWKWARD.to_string(), 2),
                (FIRST.to_string(), 0),
                (SECOND.to_string(), 1),
            ]));
            assert_eq!(arrangement(), [FIRST, SECOND, AWKWARD]);
            assert_eq!(
                placed().iter().map(|(name, _)| name).collect::<Vec<_>>(),
                [AWKWARD, FIRST, SECOND],
                "the screens are listed as they were announced"
            );
            // The same answer again is not a change: the Settings column is
            // rebuilt on every one of these.
            assert!(!note_places(vec![
                (AWKWARD.to_string(), 2),
                (FIRST.to_string(), 0),
                (SECOND.to_string(), 1),
            ]));
        });
    }

    /// Rates are printed the way somebody would say them: 60, not 60.00, and
    /// 59.94 rather than either 59 or 60.
    #[test]
    fn a_refresh_rate_reads_as_a_rate() {
        assert_eq!(hertz(60_000).as_deref(), Some("60 Hz"));
        assert_eq!(hertz(59_940).as_deref(), Some("59.94 Hz"));
        assert_eq!(hertz(74_900).as_deref(), Some("74.9 Hz"));
        assert_eq!(hertz(143_856).as_deref(), Some("143.86 Hz"));
        // Not a rate of zero: a display that reports none is not reporting one.
        assert_eq!(hertz(0), None);
    }

    // -----------------------------------------------------------------------
    // night light
    // -----------------------------------------------------------------------

    /// The controls of one screen's Night light page, however the page reaches
    /// them.
    ///
    /// The screen level collapses when only one display can be warmed, exactly
    /// as it does on the HDR page, so a test that wants the controls has to be
    /// able to find them either way.
    fn night_controls_for(name: &str) -> Vec<Entry> {
        let page = page("Night light");
        let warmable = support()
            .into_iter()
            .filter(|(_, support)| support.night_light)
            .count();
        if warmable == 1 {
            return page;
        }
        page.iter()
            .find(|entry| entry.title() == name)
            .unwrap_or_else(|| panic!("{name} is not in the Night light screen list"))
            .entries()
            .expect("a screen opens its night light")
            .to_vec()
    }

    /// One named row of that page.
    fn night_row(name: &str, title: &str) -> Entry {
        night_row_if_any(name, title)
            .unwrap_or_else(|| panic!("the night light page has no {title} row"))
    }

    /// The same, where the page not having the row at all is one of the
    /// answers being asked about.
    fn night_row_if_any(name: &str, title: &str) -> Option<Entry> {
        night_controls_for(name)
            .into_iter()
            .find(|entry| entry.title() == title)
    }

    /// The bar inside the Color temperature row.
    fn temperature_bar(name: &str) -> crate::apps::Bar {
        let row = night_row(name, "Color temperature");
        let inside = row.entries().expect("the row opens onto its bar");
        assert_eq!(inside.len(), 1, "a bar is the whole of its column");
        inside[0]
            .bar()
            .expect("and that one row is the bar")
            .clone()
    }

    /// An evening: the light on, between two hours the user chose.
    fn evening() -> NightLight {
        NightLight {
            enabled: true,
            schedule: Schedule::Hours,
            from: 21,
            until: 7,
            ..NightLight::default()
        }
    }

    /// Minutes since midnight, as the schedule counts them.
    fn at(hour: u8, minute: u8) -> u16 {
        hour as u16 * 60 + minute as u16
    }

    /// The whole of the schedule, which is the one piece of this that has to be
    /// right without a display or a clock anywhere near it.
    ///
    /// Written against minutes rather than against the time of day on purpose:
    /// a suite that read the machine's own clock would pass or fail depending
    /// on when it was run, which is the one property a test may not have.
    #[test]
    fn the_hours_of_a_schedule_wrap_past_midnight() {
        // On at nine, off at seven, and the small hours are inside it. This is
        // the case the page is mostly for and the one a window that could not
        // wrap would get exactly backwards.
        let evening = evening();
        for hour in [21, 22, 23, 0, 3, 6] {
            assert!(evening.burning_at(at(hour, 0), None), "not on at {hour}:00");
            assert!(
                evening.burning_at(at(hour, 30), None),
                "not on at {hour}:30"
            );
        }
        for hour in [7, 8, 12, 17, 20] {
            assert!(!evening.burning_at(at(hour, 0), None), "on at {hour}:00");
        }

        // An ordinary daytime window, which must not be read as its own
        // complement.
        let daytime = NightLight {
            from: 7,
            until: 21,
            ..evening
        };
        for hour in [7, 12, 20] {
            assert!(daytime.burning_at(at(hour, 0), None), "not on at {hour}:00");
        }
        for hour in [21, 23, 0, 6] {
            assert!(!daytime.burning_at(at(hour, 0), None), "on at {hour}:00");
        }

        // The end is exclusive at both, to the minute: a light that goes off at
        // seven is off at seven, and on at one minute to.
        assert!(evening.burning_at(at(6, 59), None));
        assert!(!evening.burning_at(at(7, 0), None));
        assert!(!daytime.burning_at(at(21, 0), None));

        // All day is every minute, and the switch is above all of it.
        let all_day = NightLight {
            schedule: Schedule::AllDay,
            ..evening
        };
        let switched_off = NightLight {
            enabled: false,
            ..evening
        };
        for minute in (0..MINUTES_IN_DAY).step_by(37) {
            assert!(all_day.burning_at(minute, None), "all day, not at {minute}");
            assert!(
                !switched_off.burning_at(minute, None),
                "off, on at {minute}"
            );
        }
    }

    /// The sun's own hours, which are the same window with both ends moved by
    /// the almanac rather than by the user.
    #[test]
    fn the_sun_keeps_the_hours_between_its_setting_and_its_rising() {
        use crate::sun::Sun;
        let follows = NightLight {
            enabled: true,
            schedule: Schedule::SunsetToSunrise,
            // Deliberately unlike the sun's, so a schedule reading the wrong
            // pair of hours could not accidentally agree with it.
            from: 9,
            until: 10,
            ..NightLight::default()
        };
        let summer = Some(Sun::Daily {
            sunrise: at(5, 15),
            sunset: at(20, 12),
        });
        for (hour, minute) in [(20, 12), (21, 0), (23, 59), (0, 0), (5, 14)] {
            assert!(
                follows.burning_at(at(hour, minute), summer),
                "not on at {hour}:{minute:02}"
            );
        }
        for (hour, minute) in [(5, 15), (6, 0), (12, 0), (20, 11)] {
            assert!(
                !follows.burning_at(at(hour, minute), summer),
                "on at {hour}:{minute:02}"
            );
        }

        // A day the sun does not come up is a day that is night, and one it
        // does not go down is a day that is not. Both are the truthful reading
        // of "sunset to sunrise" where neither happens.
        for minute in (0..MINUTES_IN_DAY).step_by(97) {
            assert!(
                follows.burning_at(minute, Some(Sun::NeverRises)),
                "{minute}"
            );
            assert!(
                !follows.burning_at(minute, Some(Sun::NeverSets)),
                "{minute}"
            );
        }

        // And the two hours it is not using are left exactly where they were,
        // so going back to them gives back the evening that was set.
        assert_eq!((follows.from, follows.until), (9, 10));
    }

    /// What the row above the page says out loud: which end of the window is
    /// next, and how long there is until it.
    #[test]
    fn a_schedule_says_when_it_will_next_change_its_mind() {
        use crate::sun::Sun;
        let evening = evening();
        // Inside the window, the next edge is the end of it.
        assert_eq!(evening.next_edge(at(23, 0), None), Some(at(7, 0)));
        // Outside it, the next edge is the start.
        assert_eq!(evening.next_edge(at(12, 0), None), Some(at(21, 0)));

        // The sun's edges are the sun's, to the minute.
        let follows = NightLight {
            schedule: Schedule::SunsetToSunrise,
            ..evening
        };
        let today = Some(Sun::Daily {
            sunrise: at(5, 15),
            sunset: at(20, 12),
        });
        assert_eq!(follows.next_edge(at(12, 0), today), Some(at(20, 12)));
        assert_eq!(follows.next_edge(at(22, 0), today), Some(at(5, 15)));

        // A schedule with no edges says so rather than naming one: on all day,
        // and a day at a latitude where the sun does not cross the horizon.
        let all_day = NightLight {
            schedule: Schedule::AllDay,
            ..evening
        };
        assert_eq!(all_day.next_edge(at(12, 0), None), None);
        assert_eq!(follows.next_edge(at(12, 0), Some(Sun::NeverSets)), None);
        assert_eq!(follows.next_edge(at(12, 0), None), None);
    }

    /// A window that ends where it begins is neither a whole day nor none of
    /// one, so it is empty and the page cannot offer it.
    #[test]
    fn a_window_that_ends_where_it_begins_holds_no_hours() {
        let empty = NightLight {
            until: 21,
            ..evening()
        };
        for minute in (0..MINUTES_IN_DAY).step_by(53) {
            assert!(!empty.burning_at(minute, None), "burning at {minute}");
        }

        // Which is why the Until page leaves the starting hour out: twenty-three
        // rows, and none of them the one that would mean nothing.
        with_displays(&[(FIRST, warmable())], || {
            apply_with(
                setting(FIRST, DisplayValue::NightLightSchedule(Schedule::Hours)),
                |_| {},
            );
            apply_with(setting(FIRST, DisplayValue::NightLightFrom(21)), |_| {});
            let hours: Vec<String> = night_row(FIRST, "Until")
                .entries()
                .expect("a schedule opens its hours")
                .iter()
                .map(|entry| entry.title().to_string())
                .collect();
            assert_eq!(hours.len(), 23);
            assert!(!hours.contains(&"21:00".to_string()));
            assert!(hours.contains(&"07:00".to_string()));
        });
    }

    // -----------------------------------------------------------------------
    // OLED protection
    // -----------------------------------------------------------------------

    /// The switch rows for one screen, however the page reaches them.
    fn rest_controls_for(name: &str) -> Vec<Entry> {
        let page = page("OLED protection");
        // One screen and the controls stand in the screen list's place, exactly
        // as they do on the night light page.
        if page.iter().any(|entry| entry.title() == "OLED protection") {
            return page;
        }
        page.iter()
            .find(|entry| entry.title() == name)
            .unwrap_or_else(|| panic!("{name} is not in the OLED protection screen list"))
            .entries()
            .expect("a screen opens its switch")
            .to_vec()
    }

    /// Every screen is on this page, unlike every other page under Display:
    /// what rests a display is a black sheet the compositor draws over it, and
    /// there is no connector that cannot have one.
    #[test]
    fn every_screen_can_be_rested() {
        // An SDR panel with no HDR to offer and a television with plenty. The
        // HDR page has one of them and this one has both.
        with_displays(&[(FIRST, warmable()), (SECOND, capable(PEAK))], || {
            let listed: Vec<String> = page("OLED protection")
                .iter()
                .map(|entry| entry.title().to_string())
                .collect();
            assert_eq!(listed, [FIRST, SECOND]);
        });

        // One screen, so there is no screen to choose between: the switch
        // stands in the list's place and the row above it says whose it is and
        // what it is set to.
        with_displays(&[(AWKWARD, warmable())], || {
            let row = display_row("OLED protection");
            assert_eq!(row.comment(), Some(&format!("{AWKWARD} — Off")[..]));
            assert_eq!(
                rest_controls_for(AWKWARD)
                    .iter()
                    .map(Entry::title)
                    .collect::<Vec<_>>(),
                ["OLED protection"],
                "one switch, and nothing else to set"
            );
        });
    }

    /// The switch sets the screen it is under and no other, and the page says
    /// so afterwards.
    #[test]
    fn resting_one_screen_leaves_the_other_alone() {
        with_displays(&[(FIRST, warmable()), (SECOND, warmable())], || {
            let on = rest_controls_for(FIRST)[0]
                .entries()
                .expect("the switch opens onto its two values")
                .iter()
                .find(|entry| entry.title() == "On")
                .expect("a switch has an On")
                .setting()
                .expect("a value sets something");
            assert_eq!(
                on,
                setting(intern(FIRST), DisplayValue::OledProtection(true)),
                "the row names the screen it was reached through"
            );
            assert!(apply_with(on, |_| {}));

            assert!(oled_protection_for(FIRST));
            assert!(!oled_protection_for(SECOND), "one screen, not both");
            // And the screen list says which is which without opening either.
            let comments: Vec<String> = page("OLED protection")
                .iter()
                .map(|entry| entry.comment().unwrap_or_default().to_string())
                .collect();
            assert_eq!(comments, ["On", "Off"]);
        });
    }

    /// A session whose compositor has never heard of the request offers no
    /// switch at all. A control that cannot act is worse than a page that says
    /// why, and this is a whole-session answer rather than a per-screen one.
    #[test]
    fn a_session_that_cannot_rest_a_screen_says_so() {
        with_displays(&[(FIRST, warmable()), (SECOND, warmable())], || {
            note_screen_rest(false);
            let rows = page("OLED protection");
            assert_eq!(rows.len(), 1);
            assert!(rows[0].setting().is_none(), "it sets nothing");
            assert!(
                rows[0].title().contains("No display"),
                "{:?}",
                rows[0].title()
            );
        });
    }

    /// The same three shapes the HDR page has, and the same rule about which
    /// screens are in them — but a different list of screens, which is the
    /// whole reason it is asked separately.
    #[test]
    fn the_night_light_lists_the_screens_that_can_be_warmed() {
        // An ordinary SDR panel beside an HDR television. Both can be warmed;
        // only one of them is on the HDR page.
        with_displays(&[(FIRST, warmable()), (SECOND, capable(PEAK))], || {
            let listed: Vec<String> = page("Night light")
                .iter()
                .map(|entry| entry.title().to_string())
                .collect();
            assert_eq!(listed, [FIRST, SECOND], "both screens have a ramp");

            let hdr: Vec<String> = hdr_page()
                .iter()
                .map(|entry| entry.title().to_string())
                .collect();
            assert!(
                !hdr.contains(&FIRST.to_string()),
                "an SDR panel is not on the HDR page: {hdr:?}"
            );
        });

        // One screen, so there is no screen to choose between and the controls
        // stand in its place — with the row above them saying whose they are.
        with_displays(&[(AWKWARD, warmable())], || {
            let row = display_row("Night light");
            assert!(
                row.comment().unwrap_or_default().starts_with(AWKWARD),
                "the one screen is named: {:?}",
                row.comment()
            );
            assert_eq!(
                night_controls_for(AWKWARD)
                    .iter()
                    .map(Entry::title)
                    .collect::<Vec<_>>(),
                [
                    "Night light",
                    "Color temperature",
                    "Schedule",
                    "From",
                    "Until"
                ],
                "and the time zone row is gone: it set nothing"
            );
        });

        // And none at all — a nested session, which owns no ramp. The row still
        // opens, because a row the bar refuses to step into is one that does
        // nothing when pressed, and what it says is why.
        with_displays(&[], || {
            let inside = page("Night light");
            assert_eq!(inside.len(), 1);
            assert!(inside[0].setting().is_none(), "an explanation sets nothing");
            assert_eq!(inside[0].title(), "No display can be warmed");
        });
    }

    /// The temperature is a bar, and the bar is the whole of its column: one
    /// row, no glyph, a number that reads as the value and the two steps either
    /// side of it.
    #[test]
    fn the_temperature_is_set_on_a_bar_rather_than_picked_off_a_list() {
        with_displays(&[(FIRST, warmable())], || {
            let bar = temperature_bar(FIRST);
            let started = night_light_for(FIRST).temperature;
            assert_eq!(bar.title, format!("{started} K"), "the number is the row");
            assert!(bar.comment.is_some(), "and it says what that means");
            assert!(bar.swatch.is_some(), "drawn in the light it stands for");

            // Where the handle stands is where the value stands in the range.
            let span = (NEUTRAL_KELVIN - WARMEST_ON_THE_BAR) as f32;
            let expected = (started - WARMEST_ON_THE_BAR) as f32 / span;
            assert!((bar.fill - expected).abs() < 1e-6, "{}", bar.fill);

            // One press moves it one step, and the row that comes back says so.
            let Some(up) = bar.up else {
                panic!("there is room above the default")
            };
            assert!(apply_with(up, |_| {}));
            assert_eq!(
                night_light_for(FIRST).temperature,
                started + TEMPERATURE_STEP
            );
            assert_eq!(
                temperature_bar(FIRST).title,
                format!("{} K", started + TEMPERATURE_STEP)
            );

            let Some(down) = temperature_bar(FIRST).down else {
                panic!("and room below it")
            };
            assert!(apply_with(down, |_| {}));
            assert_eq!(night_light_for(FIRST).temperature, started);
        });
    }

    /// And it is set by pointing at it as well as by stepping it: a press along
    /// the groove asks for the temperature drawn at that point.
    ///
    /// The row carries every value it can be set to rather than a range and a
    /// step, so what a press picks is a setting like any other on this page —
    /// and it is picked off the same share of the track that put the handle
    /// where the user aimed.
    #[test]
    fn a_press_along_the_bar_asks_for_the_temperature_drawn_there() {
        with_displays(&[(FIRST, warmable())], || {
            let bar = temperature_bar(FIRST);
            let kelvin_at = |level: f32| match bar.at(level) {
                Some(Setting::Display {
                    display,
                    value: DisplayValue::NightLightTemperature(kelvin),
                }) => {
                    assert_eq!(display, FIRST, "on the screen the page is about");
                    kelvin
                }
                other => panic!("a press at {level} along the track asked for {other:?}"),
            };

            // The ends of the track are the ends of the range, exactly, and the
            // middle of it is the nearest step to the middle of the range.
            assert_eq!(
                kelvin_at(0.0),
                WARMEST_ON_THE_BAR,
                "the foot is candlelight"
            );
            assert_eq!(kelvin_at(1.0), NEUTRAL_KELVIN, "and the head is daylight");
            let middle = (WARMEST_ON_THE_BAR + NEUTRAL_KELVIN) / 2;
            assert!(kelvin_at(0.5).abs_diff(middle) <= TEMPERATURE_STEP / 2);

            // The two ways of moving it move along the one range: what a
            // direction applies is one of the values a press can land on.
            for step in [bar.up, bar.down].into_iter().flatten() {
                assert!(bar.steps.contains(&step), "{step:?} is not on the track");
            }

            // What a press asks for is applied like any other row, and the row
            // that comes back is standing where the press landed — within the
            // half step that is as fine as this bar goes.
            let asked = bar.at(0.75).expect("three quarters of the way up");
            assert!(apply_with(asked, |_| {}));
            let moved = temperature_bar(FIRST);
            let step = 1.0 / (moved.steps.len() - 1) as f32;
            assert!(
                (moved.fill - 0.75).abs() <= step / 2.0 + 1e-6,
                "{}",
                moved.fill
            );

            // And a press on the step the handle is already standing on asks
            // for nothing at all: that is what aiming at a value and missing by
            // a pixel looks like, and it is not a change to apply and write
            // down.
            assert_eq!(moved.at(moved.fill), None);
        });
    }

    /// The bar stops at both ends rather than wrapping round or running past
    /// them: at the top there is no step up, at the bottom no step down.
    #[test]
    fn the_bar_stops_at_the_ends_of_its_range() {
        with_displays(&[(FIRST, warmable())], || {
            let set = |kelvin| {
                apply_with(
                    setting(FIRST, DisplayValue::NightLightTemperature(kelvin)),
                    |_| {},
                );
            };

            set(NEUTRAL_KELVIN);
            let top = temperature_bar(FIRST);
            assert_eq!(top.up, None, "nothing above daylight");
            assert!(top.down.is_some());
            assert!((top.fill - 1.0).abs() < 1e-6, "a full track");

            set(WARMEST_ON_THE_BAR);
            let bottom = temperature_bar(FIRST);
            assert_eq!(bottom.down, None, "nothing below candlelight");
            assert!(bottom.up.is_some());
            assert!(bottom.fill.abs() < 1e-6, "an empty one");

            // A value out of the bar's range is brought into it rather than
            // refused — and the bar is then somewhere on its own track.
            set(u16::MAX);
            assert_eq!(night_light_for(FIRST).temperature, NEUTRAL_KELVIN);
            set(1);
            assert_eq!(night_light_for(FIRST).temperature, WARMEST_ON_THE_BAR);
            assert!((0.0..=1.0).contains(&temperature_bar(FIRST).fill));
        });
    }

    /// Every row of every list sets the thing it names, on the screen it
    /// belongs to, and the mark moves to it.
    #[test]
    fn choosing_a_night_light_value_sets_that_value() {
        // Two screens, so the values are reached the long way and each carries
        // the name of the screen whose page it was found on.
        with_displays(&[(FIRST, warmable()), (SECOND, warmable())], || {
            for screen in [FIRST, SECOND] {
                for title in ["Night light", "Schedule"] {
                    let values = night_row(screen, title)
                        .entries()
                        .expect("a control opens its values")
                        .to_vec();
                    for entry in &values {
                        // The sun's row is a reading where this machine says no
                        // place, and a reading sets nothing on purpose.
                        let Some(chosen @ Setting::Display { display, value }) = entry.setting()
                        else {
                            assert_eq!(entry.title(), Schedule::SunsetToSunrise.title());
                            continue;
                        };
                        assert_eq!(display, screen);
                        assert!(apply_with(chosen, |_| {}));

                        let live = night_light_for(screen);
                        match value {
                            DisplayValue::NightLight(on) => assert_eq!(live.enabled, on),
                            DisplayValue::NightLightSchedule(schedule) => {
                                assert_eq!(live.schedule, schedule)
                            }
                            other => panic!("{other:?} is not on this page"),
                        }

                        // And the row that is marked afterwards is this one.
                        let marked = night_row(screen, title)
                            .entries()
                            .expect("a control opens its values")
                            .iter()
                            .find(|entry| entry.chosen())
                            .map(|entry| entry.title().to_string());
                        assert_eq!(marked.as_deref(), Some(entry.title()));
                    }
                }

                // The hours are walked on their own, because their lists only
                // exist while there is a window to keep.
                apply_with(
                    setting(screen, DisplayValue::NightLightSchedule(Schedule::Hours)),
                    |_| {},
                );
                for (title, expected) in [("From", 0u8), ("Until", 0)] {
                    let _ = expected;
                    for entry in night_row(screen, title)
                        .entries()
                        .expect("a window opens its hours")
                    {
                        let Some(chosen @ Setting::Display { value, .. }) = entry.setting() else {
                            panic!("{} sets nothing", entry.title());
                        };
                        assert!(apply_with(chosen, |_| {}));
                        let live = night_light_for(screen);
                        match value {
                            DisplayValue::NightLightFrom(hour) => assert_eq!(live.from, hour),
                            DisplayValue::NightLightUntil(hour) => assert_eq!(live.until, hour),
                            other => panic!("{other:?} is not on the {title} page"),
                        }
                    }
                }
            }

            // One screen's page is one screen's. Both have been walked down the
            // same lists and so stand at the same values; what says they are
            // separate is that moving one now leaves the other where it is.
            apply_with(
                setting(FIRST, DisplayValue::NightLightTemperature(5500)),
                |_| {},
            );
            apply_with(
                setting(SECOND, DisplayValue::NightLightSchedule(Schedule::AllDay)),
                |_| {},
            );
            assert_eq!(night_light_for(FIRST).temperature, 5500);
            assert_ne!(night_light_for(SECOND).temperature, 5500);
            assert_eq!(night_light_for(SECOND).schedule, Schedule::AllDay);
            assert_eq!(night_light_for(FIRST).schedule, Schedule::Hours);
        });
    }

    /// Changing the schedule keeps the hours it is not using, so asking for
    /// them again gives back the evening that was set rather than one the shell
    /// invented — while the two rows themselves come and go with the schedule
    /// that reads them.
    #[test]
    fn the_hours_survive_being_set_aside() {
        with_displays(&[(FIRST, warmable())], || {
            // Hours are what the switch starts on, so both rows are there and
            // both are lists of hours rather than anything to read.
            for title in ["From", "Until"] {
                let row = night_row(FIRST, title);
                let inside = row.entries().expect("the row opens");
                assert!(inside.len() >= 23, "{title} opens onto the hours");
                assert!(
                    inside.iter().all(|hour| hour.setting().is_some()),
                    "every hour on the {title} page can be chosen"
                );
            }

            apply_with(setting(FIRST, DisplayValue::NightLightFrom(22)), |_| {});
            apply_with(setting(FIRST, DisplayValue::NightLightUntil(6)), |_| {});
            let set = night_light_for(FIRST);
            assert_eq!((set.from, set.until), (22, 6));

            // Set aside, and the rows go with the schedule that was reading
            // them: two hours left on a page that is following the sun would be
            // saying something about tonight that is not true.
            apply_with(
                setting(FIRST, DisplayValue::NightLightSchedule(Schedule::AllDay)),
                |_| {},
            );
            let put_aside = night_light_for(FIRST);
            assert_eq!(put_aside.schedule, Schedule::AllDay);
            assert_eq!(
                (put_aside.from, put_aside.until),
                (22, 6),
                "the hours are remembered while they are not being kept"
            );
            for title in ["From", "Until"] {
                assert!(
                    night_row_if_any(FIRST, title).is_none(),
                    "{title} is not on a page that keeps no hours"
                );
            }

            // And asked for again, it is the same evening, on rows that are
            // back where they were.
            apply_with(
                setting(FIRST, DisplayValue::NightLightSchedule(Schedule::Hours)),
                |_| {},
            );
            assert_eq!(night_light_for(FIRST).until, 6);
            assert!(night_row_if_any(FIRST, "From").is_some());
            assert!(night_row_if_any(FIRST, "Until").is_some());
        });
    }

    /// The two hours are only ever on the page that reads them.
    ///
    /// Which is the whole of why they are hidden rather than explained: on the
    /// sun's schedule a row reading "From 22:00" is a claim about tonight, and
    /// no wording inside the row undoes two hours sitting in plain sight on the
    /// page. A schedule with no hours has three rows, not five.
    #[test]
    fn the_hours_are_only_shown_where_something_reads_them() {
        with_displays(&[(FIRST, warmable())], || {
            for (schedule, hours) in [
                (Schedule::AllDay, false),
                (Schedule::Hours, true),
                (Schedule::SunsetToSunrise, false),
            ] {
                apply_with(
                    setting(FIRST, DisplayValue::NightLightSchedule(schedule)),
                    |_| {},
                );
                let page = night_controls_for(FIRST);
                assert_eq!(
                    page.len(),
                    if hours { 5 } else { 3 },
                    "{} has the wrong number of rows",
                    schedule.title()
                );
                for title in ["From", "Until"] {
                    assert_eq!(
                        night_row_if_any(FIRST, title).is_some(),
                        hours,
                        "{title} under {}",
                        schedule.title()
                    );
                }
                // And the three that are always there are always there.
                for title in ["Night light", "Color temperature", "Schedule"] {
                    assert!(
                        night_row_if_any(FIRST, title).is_some(),
                        "{title} under {}",
                        schedule.title()
                    );
                }
            }
        });
    }

    /// The two hours may never be the same, whichever end is moved onto the
    /// other — a press has to do something, and refusing it silently would be a
    /// row that looks broken.
    #[test]
    fn the_two_ends_of_a_window_never_meet() {
        with_displays(&[(FIRST, warmable())], || {
            apply_with(
                setting(FIRST, DisplayValue::NightLightSchedule(Schedule::Hours)),
                |_| {},
            );
            apply_with(setting(FIRST, DisplayValue::NightLightFrom(21)), |_| {});
            apply_with(setting(FIRST, DisplayValue::NightLightUntil(7)), |_| {});

            // Moving the start onto the end pushes the end along rather than
            // leaving a window that holds no hours.
            apply_with(setting(FIRST, DisplayValue::NightLightFrom(7)), |_| {});
            let moved = night_light_for(FIRST);
            assert_eq!(moved.from, 7);
            assert_ne!(moved.until, 7);

            // And the end can never be put onto the start, because the page
            // does not offer it; a value that arrived anyway is ignored.
            apply_with(setting(FIRST, DisplayValue::NightLightUntil(7)), |_| {});
            assert_ne!(night_light_for(FIRST).until, 7);
        });
    }

    /// What the compositor is asked for is an answer, not a schedule: whether
    /// the light should be burning at this moment, and how warm.
    #[test]
    fn what_is_sent_is_the_answer_rather_than_the_schedule() {
        with_displays(&[(FIRST, warmable())], || {
            // Off is off, and the temperature still travels — the compositor
            // clamps and encodes it, and a shell that sent nothing would have
            // to be told twice when it came on.
            apply_with(setting(FIRST, DisplayValue::NightLight(false)), |_| {});
            apply_with(
                setting(FIRST, DisplayValue::NightLightTemperature(2700)),
                |_| {},
            );
            assert_eq!(night_light_now(FIRST), (false, 2700));

            // On with no schedule is on, whatever hour it happens to be while
            // this runs. That is the one answer a test may assert without
            // reading the clock — and the schedule has to be *said*, which is
            // what this was missing. It asserted the same thing having set
            // nothing, on the assumption that a fresh entry has no schedule;
            // [`NightLight::default`] is an evening, ready-made, so what the
            // assertion really tested was that the machine running it was
            // between ten at night and six in the morning. It passed for
            // whoever wrote it and failed every day after breakfast.
            apply_with(
                setting(FIRST, DisplayValue::NightLightSchedule(Schedule::AllDay)),
                |_| {},
            );
            apply_with(setting(FIRST, DisplayValue::NightLight(true)), |_| {});
            assert_eq!(night_light_now(FIRST), (true, 2700));

            // And with a schedule, the answer is the schedule's — asserted
            // against the time the machine says it is rather than against one
            // written here, which is the only way this can be right on a
            // machine in any time zone.
            apply_with(
                setting(FIRST, DisplayValue::NightLightTemperature(4000)),
                |_| {},
            );
            apply_with(
                setting(FIRST, DisplayValue::NightLightSchedule(Schedule::Hours)),
                |_| {},
            );
            apply_with(setting(FIRST, DisplayValue::NightLightFrom(21)), |_| {});
            apply_with(setting(FIRST, DisplayValue::NightLightUntil(7)), |_| {});
            let expected = match local_time() {
                Some(now) => night_light_for(FIRST).burning_at(now.minute_of_day(), sun_today()),
                // No clock is no schedule, so the switch means what it says.
                None => true,
            };
            assert_eq!(night_light_now(FIRST), (expected, 4000));
        });
    }

    /// Walking down the values changes nothing. Every row here reconfigures a
    /// connector, and a filter that came on as the cursor passed over it would
    /// be a page that could not be read.
    #[test]
    fn walking_over_a_night_light_value_changes_nothing() {
        with_displays(&[(FIRST, warmable())], || {
            for title in [
                "Night light",
                "Color temperature",
                "Schedule",
                "From",
                "Until",
            ] {
                for entry in night_row(FIRST, title).entries().unwrap() {
                    preview(entry.setting());
                }
            }
            assert_eq!(night_light_for(FIRST), NightLight::default());
        });
    }

    /// The moon is kept to the night light, as the HDR badge is kept to HDR,
    /// and the clock to the rows that are about a time.
    ///
    /// The badge test next door exists because a read-only explanation once
    /// borrowed the HDR glyph and made unrelated pages look like HDR at a
    /// glance. This is the same rule for the same reason.
    #[test]
    fn the_night_light_glyphs_stay_on_their_own_rows() {
        with_displays(&[(FIRST, warmable())], || {
            let row = display_row("Night light");
            assert_eq!(row.icon(), Some(icons::SETTING_NIGHT_LIGHT));

            let worn = |title: &str| night_row(FIRST, title).icon().map(str::to_string);
            assert_eq!(
                worn("Night light").as_deref(),
                Some(icons::SETTING_NIGHT_LIGHT),
                "the switch wears the setting's own mark"
            );
            for hours in ["Schedule", "From", "Until"] {
                assert_eq!(
                    worn(hours).as_deref(),
                    Some(icons::SETTING_SCHEDULE),
                    "{hours} is about a time, not a moon"
                );
            }
            // The bar wears nothing at all: the track is the drawing, and the
            // name of the setting is on the row it was opened from.
            let bar = night_row(FIRST, "Color temperature");
            assert_eq!(bar.entries().unwrap()[0].icon(), None);
            // And nothing here borrows the HDR badge.
            for title in [
                "Night light",
                "Color temperature",
                "Schedule",
                "From",
                "Until",
            ] {
                assert_ne!(worn(title).as_deref(), Some(icons::SETTING_HDR));
            }
        });
    }

    /// What is written is what is read, and a schedule that means nothing is
    /// dropped with the rest of the file intact.
    #[test]
    fn the_night_light_survives_the_file() {
        let held = LOCK.lock().unwrap_or_else(|held| held.into_inner());
        let saved = take_settings();

        let written = NightLight {
            enabled: true,
            temperature: 2700,
            schedule: Schedule::Hours,
            from: 22,
            until: 6,
        };
        NIGHT.lock().unwrap().insert(FIRST.to_string(), written);
        let body = toml::to_string_pretty(&stored()).unwrap();
        // The keys the preamble documents, spelled the way it spells them.
        for key in [
            "night-light",
            "night-light-temperature",
            "night-light-schedule",
            "night-light-from",
            "night-light-until",
        ] {
            assert!(body.contains(key), "{key} is not in the file:\n{body}");
        }

        NIGHT.lock().unwrap().clear();
        adopt(toml::from_str(&body).unwrap());
        assert_eq!(night_light_for(FIRST), written);
        // A screen the file says nothing about is unwarmed, not a copy of one
        // that is: there is nothing inherited behind this setting.
        assert_eq!(night_light_for(SECOND), NightLight::default());

        // A hand-edited window that ends where it begins keeps no hours at all,
        // rather than keeping ones nothing can satisfy — and the rest of that
        // display's settings survive it.
        adopt(
            toml::from_str(
                r#"
                [display."TEST-OUT-1"]
                night-light = true
                night-light-temperature = 3400
                night-light-schedule = "hours"
                night-light-from = 9
                night-light-until = 9
                "#,
            )
            .unwrap(),
        );
        let read = night_light_for(FIRST);
        assert!(read.enabled);
        assert_eq!(read.temperature, 3400);
        assert_eq!(
            read.schedule,
            Schedule::AllDay,
            "an empty window is no window"
        );

        // A word this shell does not have is dropped, and what is left is the
        // schedule a display that had never been set would have: there is no
        // way to guess what was meant, and the shell's own answer is a better
        // one than any of the three picked at random.
        adopt(
            toml::from_str(
                r#"
                [display."TEST-OUT-1"]
                night-light = true
                night-light-schedule = "whenever-it-feels-like-it"
                "#,
            )
            .unwrap(),
        );
        assert_eq!(
            night_light_for(FIRST).schedule,
            NightLight::default().schedule
        );

        // And an hour or a temperature out of range is brought into it rather
        // than taking the file down.
        adopt(
            toml::from_str(
                r#"
                [display."TEST-OUT-1"]
                night-light = true
                night-light-temperature = 60000
                night-light-from = 99
                "#,
            )
            .unwrap(),
        );
        let read = night_light_for(FIRST);
        assert_eq!(read.temperature, NEUTRAL_KELVIN);
        assert_eq!(read.from, 23);

        put_back(saved);
        drop(held);
    }

    /// A location written into the settings file is what the sun is worked out
    /// for, and it survives the file being written back.
    #[test]
    fn a_location_in_the_file_is_where_the_sun_is_worked_out_for() {
        let held = LOCK.lock().unwrap_or_else(|held| held.into_inner());
        let saved = take_settings();
        let was = crate::sun::written_location();

        // A place nobody's machine is set to.
        adopt(
            toml::from_str(
                r#"
                night-light-latitude = -33.87
                night-light-longitude = 151.21
                "#,
            )
            .unwrap(),
        );
        let at = crate::sun::location().expect("a written location is a location");
        assert!((at.latitude + 33.87).abs() < 1e-9);
        assert!((at.longitude - 151.21).abs() < 1e-9);
        // And it goes back into the file, or the next change to anything else
        // would drop it.
        let body = toml::to_string_pretty(&stored()).unwrap();
        assert!(body.contains("night-light-latitude"), "{body}");

        // Half a coordinate is not a place, and neither is one off the earth.
        adopt(toml::from_str("night-light-latitude = 52.25").unwrap());
        assert_eq!(crate::sun::written_location(), None);
        adopt(
            toml::from_str(
                r#"
                night-light-latitude = 999.0
                night-light-longitude = 0.0
                "#,
            )
            .unwrap(),
        );
        assert_eq!(crate::sun::written_location(), None);

        crate::sun::set_location(was);
        put_back(saved);
        drop(held);
    }

    /// The clock is the machine's own, read through the C library, and nothing
    /// here may depend on which machine that is.
    ///
    /// So what is asserted is only what is true wherever it is run: that the
    /// reading is a time of day on a date, and that the offset is one the world
    /// has. A suite that checked the hour would be one that passed until it was
    /// run somewhere else.
    #[test]
    fn the_clock_is_a_time_of_day_wherever_it_is_read() {
        let Some(now) = local_time() else {
            // A machine with no time zone data at all. Legal, and the shell has
            // an answer for it; there is nothing further to check.
            return;
        };
        assert!(now.hour <= 23);
        assert!(now.minute <= 59);
        assert!(now.yday <= 365);
        assert!(now.year > 1970, "{}", now.year);
        // The widest any zone has ever been from UTC is fourteen hours.
        assert!(now.offset.abs() <= 14 * 3600, "{}", now.offset);
        assert_eq!(
            now.minute_of_day(),
            now.hour as u16 * 60 + now.minute as u16
        );
        assert!(now.minute_of_day() < MINUTES_IN_DAY);
    }

    /// How a time and a window are said, which is the same twenty-four hour
    /// clock everywhere: a light set for 9 is otherwise set for nine in the
    /// morning half the time.
    #[test]
    fn a_window_says_how_long_it_lasts() {
        assert_eq!(window_length(21, 7), "10 hours of night light");
        assert_eq!(window_length(7, 21), "14 hours of night light");
        assert_eq!(window_length(23, 0), "One hour of night light");
        assert_eq!(window_length(0, 23), "23 hours of night light");

        assert_eq!(hour_title(0), "00:00");
        assert_eq!(hour_title(9), "09:00");
        assert_eq!(hour_title(21), "21:00");
        // The sun does not keep hours, so its times carry minutes.
        assert_eq!(clock_title(at(20, 12)), "20:12");
        assert_eq!(clock_title(at(5, 5)), "05:05");
        assert_eq!(clock_title(0), "00:00");
        assert_eq!(clock_title(MINUTES_IN_DAY), "23:59", "clamped into the day");
    }

    /// Every temperature the bar can be set to has a word for what it is, and
    /// a colour that is warmer the further down the track it stands.
    #[test]
    fn every_temperature_on_the_bar_has_a_colour_and_a_word() {
        let mut previous: Option<Color> = None;
        let mut kelvin = NEUTRAL_KELVIN;
        while kelvin >= WARMEST_ON_THE_BAR {
            assert!(!warmth_note(kelvin).is_empty(), "{kelvin} K says nothing");
            let tint = tint_of(kelvin);
            let [red, green, blue] = [tint.0 >> 16 & 0xff, tint.0 >> 8 & 0xff, tint.0 & 0xff];
            assert_eq!(red, 255, "{kelvin} K moves red");
            assert!(blue <= green, "{kelvin} K is not warm: {tint:?}");
            if let Some(cooler) = previous {
                assert!(
                    tint.0 & 0xff <= cooler.0 & 0xff,
                    "{kelvin} K is bluer than the step above it"
                );
            }
            previous = Some(tint);
            kelvin -= TEMPERATURE_STEP;
        }
        // Daylight is white, which is what "no filter" has to look like.
        assert_eq!(tint_of(NEUTRAL_KELVIN).0 >> 16 & 0xff, 255);

        // Only the head of the track claims to do nothing. One step below it
        // the picture *has* been changed, and a row saying otherwise there
        // would be saying the setting had not taken.
        let none_at_all = warmth_note(NEUTRAL_KELVIN);
        assert_ne!(
            warmth_note(NEUTRAL_KELVIN - TEMPERATURE_STEP),
            none_at_all,
            "one step off daylight is not daylight"
        );
        for step in 1..=44u16 {
            let kelvin = NEUTRAL_KELVIN - step * TEMPERATURE_STEP;
            assert_ne!(
                warmth_note(kelvin),
                none_at_all,
                "{kelvin} K warms nothing?"
            );
        }

        // And the words only ever get warmer: walking the bar one way never
        // reads as turning back.
        let mut bands: Vec<&str> = Vec::new();
        let mut kelvin = NEUTRAL_KELVIN;
        while kelvin >= WARMEST_ON_THE_BAR {
            if bands.last() != Some(&warmth_note(kelvin)) {
                assert!(
                    !bands.contains(&warmth_note(kelvin)),
                    "{kelvin} K goes back to a band the bar has already left"
                );
                bands.push(warmth_note(kelvin));
            }
            kelvin -= TEMPERATURE_STEP;
        }
        assert!(
            bands.len() >= 5,
            "the range is described, not labelled once"
        );
    }

    /// The System page's rows, in the order it offers them.
    /// The Games row of the Settings column, as the bar shows it.
    fn games_row() -> Entry {
        column()
            .into_iter()
            .find(|entry| entry.title() == "Games")
            .expect("the Settings column has a Games row")
    }

    /// The Steam row of the Games page, which is what the page's own comment
    /// hangs on.
    fn steam_row() -> Entry {
        games_row()
            .entries()
            .expect("Games opens onto its own page")
            .iter()
            .find(|entry| entry.title() == "Steam")
            .expect("and Steam is on it")
            .clone()
    }

    /// And what is under it.
    fn steam_page() -> Vec<Entry> {
        steam_row()
            .entries()
            .expect("Steam opens onto its own page")
            .to_vec()
    }

    fn system_page() -> Vec<Entry> {
        column()
            .into_iter()
            .find(|entry| entry.title() == "System")
            .expect("the Settings column has a System row")
            .entries()
            .expect("which opens onto its own page")
            .to_vec()
    }

    /// The bar the Application scaling row opens onto.
    fn scaling_bar() -> crate::apps::Bar {
        let row = system_page()
            .into_iter()
            .find(|entry| entry.title() == "Application scaling")
            .expect("the System page offers the scale");
        let inside = row.entries().expect("the row opens onto its bar");
        assert_eq!(inside.len(), 1, "a bar is the whole of its column");
        inside[0]
            .bar()
            .expect("and that one row is the bar")
            .clone()
    }

    /// Set the scale, the way a press on the bar sets it.
    fn set_scale(percent: u16) {
        assert!(apply_with(Setting::AppScale(percent), |_| {}));
    }

    /// How large applications draw themselves is set by sliding rather than by
    /// picking, like the night light's temperature and for the same reason:
    /// what is being chosen is a scale and not a set of alternatives.
    #[test]
    fn the_scale_is_set_on_a_bar_rather_than_picked_off_a_list() {
        with_displays(&[], || {
            let bar = scaling_bar();
            let started = app_scale();
            assert_eq!(started, NATURAL_SCALE, "a session starts at one to one");
            assert_eq!(bar.title, format!("{started}%"), "the number is the row");
            assert!(bar.comment.is_some(), "and it says what that means");
            assert_eq!(
                bar.swatch, None,
                "a size has no colour to be drawn in, unlike a temperature"
            );
            assert!(bar.fill.abs() < 1e-6, "the handle starts at the foot");

            // One press moves it one step, and the row that comes back says so.
            let Some(up) = bar.up else {
                panic!("there is room above one to one")
            };
            assert!(apply_with(up, |_| {}));
            assert_eq!(app_scale(), started + SCALE_STEP);
            assert_eq!(
                scaling_bar().title,
                format!("{}%", started + SCALE_STEP),
                "and the row reads as where it now stands"
            );

            // The row above it carries the same answer, so a user who has
            // walked back out of the bar can still read what it is set to.
            let row = system_page()
                .into_iter()
                .find(|entry| entry.title() == "Application scaling")
                .expect("the row is still there");
            assert!(
                row.comment().is_some_and(
                    |comment| comment.starts_with(&format!("{}%", started + SCALE_STEP))
                ),
                "the row says the scale: {:?}",
                row.comment()
            );

            let Some(down) = scaling_bar().down else {
                panic!("and room below it again")
            };
            assert!(apply_with(down, |_| {}));
            assert_eq!(app_scale(), started);
        });
    }

    /// The Startup category page is the bar itself: one row per column this
    /// machine has, in the order they stand in, each wearing its own mark. And
    /// the row it was opened from wears the mark of whichever is chosen.
    ///
    /// Games unless somebody has said otherwise, which is the whole reason the
    /// setting exists: the shell opened on the first column with anything in it
    /// before, and on nearly every machine that is System.
    #[test]
    fn the_startup_page_is_the_bar_with_one_of_them_marked() {
        with_displays(&[], || {
            let bar = crate::apps::every_column();
            let page = || {
                system(&bar)
                    .entries()
                    .expect("System opens a column")
                    .iter()
                    .find(|entry| entry.title() == "Startup category")
                    .expect("the System page offers where the shell opens")
                    .clone()
            };

            let opened = page();
            let rows = opened.entries().expect("it opens onto the columns");
            assert_eq!(
                rows.iter().map(Entry::title).collect::<Vec<_>>(),
                bar.iter().map(|column| column.title).collect::<Vec<_>>(),
                "the page is the bar, in the bar's own order"
            );
            for (row, column) in rows.iter().zip(&bar) {
                assert_eq!(
                    row.icon(),
                    Some(column.icon),
                    "{} should wear its own mark",
                    column.title
                );
            }

            fn marked(rows: &[Entry]) -> Vec<&str> {
                rows.iter()
                    .filter(|row| row.chosen())
                    .map(Entry::title)
                    .collect()
            }
            assert_eq!(
                marked(rows),
                ["Games"],
                "a shell nobody has asked opens on the games"
            );
            assert_eq!(opened.icon(), Some(icons::CATEGORY_GAMES));

            // A press moves the mark and writes the id down. Never through
            // `apply`, for the reason the volume test gives.
            let waydroid = rows
                .iter()
                .find(|row| row.title() == "Waydroid")
                .expect("Waydroid is a column the shell can have");
            let mut persisted = None;
            assert!(apply_with(
                waydroid.setting().expect("the row sets something"),
                |stored| persisted = stored.startup_category.clone(),
            ));
            assert_eq!(persisted.as_deref(), Some("waydroid"));

            let opened = page();
            assert_eq!(
                marked(opened.entries().expect("it still opens")),
                ["Waydroid"]
            );
            assert_eq!(
                opened.icon(),
                Some(icons::CATEGORY_WAYDROID),
                "and the row it was opened from wears what was chosen"
            );

            // Highlighting one changes nothing. There is nothing to preview —
            // what it sets is where the next cursor is built — and a bar that
            // walked off under somebody reading the list would be worse than
            // no preview at all.
            preview(rows[0].setting());
            assert_eq!(startup_category(), "waydroid");
        });
    }

    /// A column the setting names and this machine has not got is still offered,
    /// still marked, and says why it is not there.
    ///
    /// The case is a Steam account signed out, or a package removed. A page
    /// with nothing marked on it would be the shell losing the answer in front
    /// of the person who gave it.
    #[test]
    fn a_column_that_is_not_here_is_still_the_answer() {
        with_displays(&[], || {
            note_startup_category(Some(crate::apps::steam_column()));
            let bar = [crate::apps::Column {
                id: "system",
                title: "System",
                icon: icons::CATEGORY_SYSTEM,
            }];
            let opened = system(&bar)
                .entries()
                .expect("System opens a column")
                .iter()
                .find(|entry| entry.title() == "Startup category")
                .expect("the System page offers where the shell opens")
                .clone();
            let rows = opened.entries().expect("it opens onto the columns");

            assert_eq!(
                rows.iter().map(Entry::title).collect::<Vec<_>>(),
                ["System", crate::apps::steam_title()],
                "the bar, and then the answer that is not on it"
            );
            let absent = rows.last().expect("the row that is not here");
            assert!(absent.chosen(), "it is still what was chosen");
            assert_eq!(absent.comment(), Some("Not on this machine just now"));
            assert_eq!(
                rows[0].comment(),
                None,
                "and a column that is here says nothing"
            );
            assert_eq!(opened.icon(), Some(icons::CATEGORY_STEAM));
        });
    }

    /// The System page offers where the shell opens, the scale, and then the
    /// page about the machine — and the last of them is a door rather than a
    /// setting.
    ///
    /// Everything this asserts is something a press depends on. The row carries
    /// no [`Setting`], so choosing it moves no mark and writes no file; it opens
    /// no column, so Right and Accept cannot walk into an empty one; and it
    /// starts nothing, so the bar must not treat it as a tile. What it *does*
    /// is answered by `Shell::start_selection`, which asks
    /// [`crate::apps::Entry::facts`] — the one thing here that must keep
    /// answering with something.
    #[test]
    fn the_system_page_ends_with_a_door_rather_than_a_setting() {
        with_displays(&[], || {
            let page = system_page();
            let titles: Vec<&str> = page.iter().map(|entry| entry.title()).collect();
            assert_eq!(
                titles,
                [
                    "Startup category",
                    "Application scaling",
                    "Picture-in-Picture",
                    "Button hints",
                    "System information"
                ],
                "the settings come first and the page to read comes last"
            );

            let row = page.last().expect("the page has a last row");
            assert_eq!(
                row.facts().map(|facts| &facts.about),
                Some(&crate::apps::About::Machine),
                "the press is answered by the panel, read at the moment of it"
            );
            assert_eq!(row.setting(), None, "there is nothing here to set");
            assert!(!row.chosen(), "and therefore no mark to carry");
            assert!(row.entries().is_none(), "it opens no column of the bar");
            assert!(!row.starts_something(), "and forks nothing");
            assert_eq!(
                row.icon(),
                Some(icons::SETTING_INFO),
                "the shell's read-only mark, not the System page's own chip"
            );
            assert!(row.comment().is_some(), "and it says what is behind it");
        });
    }

    /// Nothing below the size an application chose for itself.
    ///
    /// The one property of this bar that is not the temperature bar's: that one
    /// has a range with a neutral end, and this one has a *floor*. An
    /// application asked to draw its interface smaller than it chose is not
    /// something a screen looked at from an armchair ever wants, so the foot of
    /// the track is one to one and there is no step below it — by any route.
    #[test]
    fn nothing_is_drawn_smaller_than_the_application_chose() {
        with_displays(&[], || {
            let bar = scaling_bar();
            assert_eq!(bar.down, None, "nothing below one to one");
            assert!(bar.up.is_some());
            assert!(
                bar.steps.iter().all(
                    |step| matches!(step, Setting::AppScale(percent) if *percent >= NATURAL_SCALE)
                ),
                "a press along the groove can never ask for less"
            );

            // Nor by asking for it outright: a hand-edited file or an older
            // shell is answered with the nearest size that means something,
            // exactly as an out-of-range colour temperature is.
            set_scale(50);
            assert_eq!(app_scale(), NATURAL_SCALE);
            set_scale(0);
            assert_eq!(app_scale(), NATURAL_SCALE);
            set_scale(u16::MAX);
            assert_eq!(app_scale(), LARGEST_SCALE);
            assert!((0.0..=1.0).contains(&scaling_bar().fill));
        });
    }

    /// And the top of the range is a step the bar can actually stand on, with
    /// nothing above it.
    #[test]
    fn the_scaling_bar_stops_at_the_top_of_its_range() {
        with_displays(&[], || {
            set_scale(LARGEST_SCALE);
            let top = scaling_bar();
            assert_eq!(top.up, None, "nothing past the largest");
            assert!(top.down.is_some());
            assert!((top.fill - 1.0).abs() < 1e-6, "a full track");
            assert_eq!(
                top.steps.last(),
                Some(&Setting::AppScale(LARGEST_SCALE)),
                "the head of the track is the end of the range"
            );
            // Which needs the step to divide the range: a bar whose last step
            // fell short would have a head the user could not land on.
            assert_eq!((LARGEST_SCALE - NATURAL_SCALE) % SCALE_STEP, 0);
        });
    }

    /// A press along the groove asks for the size drawn at that point, the way
    /// a press along the temperature bar asks for the temperature there.
    #[test]
    fn a_press_along_the_scaling_bar_asks_for_the_size_drawn_there() {
        with_displays(&[], || {
            let bar = scaling_bar();
            let percent_at = |level: f32| match bar.at(level) {
                Some(Setting::AppScale(percent)) => percent,
                other => panic!("a press at {level} along the track asked for {other:?}"),
            };

            assert_eq!(
                percent_at(1.0),
                LARGEST_SCALE,
                "the head is as far as it goes"
            );
            let middle = (NATURAL_SCALE + LARGEST_SCALE) / 2;
            assert!(percent_at(0.5).abs_diff(middle) <= SCALE_STEP / 2 + 1);

            // The two ways of moving it move along the one range.
            for step in [bar.up, bar.down].into_iter().flatten() {
                assert!(bar.steps.contains(&step), "{step:?} is not on the track");
            }

            // What a press asks for is applied like any other row, and the row
            // that comes back is standing where the press landed.
            let asked = bar.at(0.75).expect("three quarters of the way up");
            assert!(apply_with(asked, |_| {}));
            let moved = scaling_bar();
            let step = 1.0 / (moved.steps.len() - 1) as f32;
            assert!(
                (moved.fill - 0.75).abs() <= step / 2.0 + 1e-6,
                "{}",
                moved.fill
            );

            // And a press on the step the handle is already on asks for
            // nothing: that is aiming at the handle and missing by a pixel.
            assert_eq!(moved.at(moved.fill), None);
        });
    }

    /// Every size the bar can be set to has a word for what it is, and the
    /// words only ever grow: walking the bar one way never reads as turning
    /// back.
    #[test]
    fn every_scale_on_the_bar_has_a_word() {
        // Only the foot of the track claims to leave applications alone. One
        // step above it they *have* been changed, however slightly, and a row
        // saying otherwise there would be saying the setting had not taken.
        let its_own_size = scale_note(NATURAL_SCALE);
        assert_ne!(scale_note(NATURAL_SCALE + SCALE_STEP), its_own_size);

        let mut bands: Vec<&str> = Vec::new();
        let mut percent = NATURAL_SCALE;
        while percent <= LARGEST_SCALE {
            assert!(!scale_note(percent).is_empty(), "{percent}% says nothing");
            if percent > NATURAL_SCALE {
                assert_ne!(
                    scale_note(percent),
                    its_own_size,
                    "{percent}% is not one to one"
                );
            }
            if bands.last() != Some(&scale_note(percent)) {
                assert!(
                    !bands.contains(&scale_note(percent)),
                    "{percent}% goes back to a band the bar has already left"
                );
                bands.push(scale_note(percent));
            }
            percent += SCALE_STEP;
        }
        assert!(
            bands.len() >= 5,
            "the range is described, not labelled once"
        );
    }

    /// It survives the file, and a file that says nothing about it leaves every
    /// application at its own size.
    #[test]
    fn the_scale_survives_the_file() {
        with_displays(&[], || {
            set_scale(150);

            let written = stored();
            assert_eq!(written.application_scale, Some(150));
            let body = toml::to_string_pretty(&written).unwrap();
            assert!(body.contains("application-scale"), "{body}");

            *APP_SCALE.lock().unwrap() = NATURAL_SCALE;
            adopt(toml::from_str(&body).unwrap());
            assert_eq!(app_scale(), 150, "and it comes back where it was left");

            // Session-wide: it is not written into any display's section, and
            // it is not read out of one either.
            assert!(
                !body.contains("[display."),
                "the scale put a screen in the file:\n{body}"
            );

            // A silent file answers nothing for the user, as it does for the
            // Start music: what a session with no file comes up at is one to
            // one, which is where the value starts rather than something the
            // reader has to put back.
            adopt(Stored::default());
            assert_eq!(app_scale(), 150);

            // A hand-edited size outside the range is clamped rather than
            // refused, and the same way the compositor would clamp it, so the
            // file and the screen agree about what is in force.
            adopt(Stored {
                application_scale: Some(10),
                ..Stored::default()
            });
            assert_eq!(app_scale(), NATURAL_SCALE);
            adopt(Stored {
                application_scale: Some(9000),
                ..Stored::default()
            });
            assert_eq!(app_scale(), LARGEST_SCALE);
        });
    }

    /// The floating window's page: a switch, a size and a corner, in that order,
    /// and the page's own comment saying what all three are.
    ///
    /// The order is the argument. Whether such a window floats at all is the
    /// question somebody came here with; how large it is and where it sits are
    /// only questions once the answer to the first is yes.
    #[test]
    fn the_floating_window_is_a_switch_and_two_answers() {
        with_displays(&[], || {
            let Some(page) = column()
                .into_iter()
                .find(|entry| entry.title() == "System")
                .and_then(|system| {
                    system.entries().and_then(|rows| {
                        rows.iter()
                            .find(|row| row.title() == "Picture-in-Picture")
                            .cloned()
                    })
                })
            else {
                panic!("System offers the floating window")
            };
            assert_eq!(page.comment(), Some("On, medium, top right"));
            assert_eq!(page.icon(), Some(icons::SETTING_PIP));

            let rows = page.entries().expect("it opens onto its three rows");
            let titles: Vec<&str> = rows.iter().map(|row| row.title()).collect();
            assert_eq!(titles, ["Picture-in-Picture", "Size", "Placement"]);

            // The switch is marked where the setting is, and both halves say
            // what they mean rather than only Off and On.
            let switch = rows[0].entries().expect("the switch is a pair of values");
            assert_eq!(
                switch.iter().map(|row| row.chosen()).collect::<Vec<_>>(),
                [false, true],
                "it floats until somebody says otherwise"
            );
            assert!(switch.iter().all(|row| row.comment().is_some()));

            // And the corner rows are drawn rather than spelled, each with its
            // own corner — which is the whole reason they are drawn.
            let corners = rows[2].entries().expect("four corners");
            let marks: Vec<Option<&str>> = corners.iter().map(|row| row.icon()).collect();
            assert_eq!(
                marks,
                [
                    Some(icons::SETTING_PIP_TOP_LEFT),
                    Some(icons::SETTING_PIP_TOP_RIGHT),
                    Some(icons::SETTING_PIP_BOTTOM_LEFT),
                    Some(icons::SETTING_PIP_BOTTOM_RIGHT),
                ]
            );
            // The row they hang on wears the corner that is chosen, so the list
            // is headed by the answer rather than by a picture of all four.
            assert_eq!(rows[2].icon(), Some(icons::SETTING_PIP_TOP_RIGHT));
        });
    }

    /// Each of the three is set on its own and the other two are left alone,
    /// which is what makes them three rows rather than one.
    #[test]
    fn each_half_of_the_floating_window_is_set_on_its_own() {
        with_displays(&[], || {
            assert!(apply_with(
                Setting::PictureInPicture(PipValue::Place(pip::Place::BottomLeft)),
                |_| {}
            ));
            assert_eq!(
                picture_in_picture(),
                Pip {
                    place: pip::Place::BottomLeft,
                    ..Pip::DEFAULT
                }
            );
            assert!(apply_with(
                Setting::PictureInPicture(PipValue::Size(pip::Size::Small)),
                |_| {}
            ));
            assert!(apply_with(
                Setting::PictureInPicture(PipValue::Floating(false)),
                |_| {}
            ));
            assert_eq!(
                picture_in_picture(),
                Pip {
                    floating: false,
                    size: pip::Size::Small,
                    place: pip::Place::BottomLeft,
                }
            );
            // And with the whole of it off, the page says so and stops
            // describing a window nobody can see.
            assert_eq!(floating_summary(picture_in_picture()), "Off");
        });
    }

    /// It survives the file, and a file that says nothing leaves it floating.
    #[test]
    fn the_floating_window_survives_the_file() {
        with_displays(&[], || {
            assert!(apply_with(
                Setting::PictureInPicture(PipValue::Size(pip::Size::Large)),
                |_| {}
            ));
            assert!(apply_with(
                Setting::PictureInPicture(PipValue::Place(pip::Place::BottomRight)),
                |_| {}
            ));

            let body = toml::to_string_pretty(&stored()).unwrap();
            assert!(
                body.contains("picture-in-picture-size = \"large\""),
                "{body}"
            );
            assert!(
                body.contains("picture-in-picture-place = \"bottom-right\""),
                "{body}"
            );

            *PICTURE_IN_PICTURE.lock().unwrap() = Pip::DEFAULT;
            adopt(toml::from_str(&body).unwrap());
            assert_eq!(
                picture_in_picture(),
                Pip {
                    floating: true,
                    size: pip::Size::Large,
                    place: pip::Place::BottomRight,
                }
            );

            // Session-wide: no screen went into the file for it.
            assert!(!body.contains("[display."), "{body}");

            // A file that says nothing leaves what is in force alone, as it
            // does for the scale and the Start music.
            adopt(Stored::default());
            assert_eq!(picture_in_picture().size, pip::Size::Large);

            // And a hand-edited word nobody has heard of is ignored on its own,
            // without taking the half beside it down with it.
            adopt(Stored {
                picture_in_picture_size: Some("enormous".to_string()),
                picture_in_picture_place: Some("top-left".to_string()),
                ..Stored::default()
            });
            assert_eq!(
                picture_in_picture(),
                Pip {
                    floating: true,
                    size: pip::Size::Large,
                    place: pip::Place::TopLeft,
                }
            );
        });
    }

    /// An emulator that handed over no settings is still on the page.
    ///
    /// Play! is the one: it makes no answer at all until a game is loaded, so
    /// the helper has nothing to report but the core's name. Left off the page
    /// entirely — which is what happened — somebody who had just installed it
    /// went looking for their PlayStation 2 emulator and found nothing, which
    /// reads as the install having failed rather than as an emulator with
    /// nothing to set.
    #[test]
    fn an_emulator_with_nothing_to_set_is_still_on_the_page() {
        let _held = crate::retroarch::GLOBALS
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        // Nothing this core cannot start without, which is what puts it down
        // the branch being tested rather than the one with a BIOS row on it.
        crate::retroarch::set_firmware(Vec::new());
        let silent: crate::retroarch::CoreOptions = serde_json::from_str(
            r#"{"protocol":1,"core":"pcsx2","display":"LRPS2","categories":[],"options":[]}"#,
        )
        .expect("the helper's own wire format");

        let Entry::Choice(row) = core_page(&silent) else {
            panic!("a core with nothing in it must not open as a folder");
        };
        assert_eq!(row.title, "LRPS2");
        assert!(!row.acts, "there is nothing here to press");
        let note = row.comment.expect("a row that says why it is empty");
        // The one thing this row exists to say. An emulator that declares
        // nothing until a game is loaded is not an emulator without settings,
        // and a row reading "no settings" would send somebody looking for a
        // page that is one press of a game away.
        assert!(
            note.contains("RetroArch's own menu"),
            "it says where they actually are, and got {note:?}"
        );
        // And does not promise that playing something brings them here. A core
        // that answers neither the cheap ask nor the deep one only speaks with
        // a real game in it, and nothing the shell does afterwards will hear it.
        assert!(
            !note.to_lowercase().contains("play a game"),
            "and promises nothing it cannot do, but got {note:?}"
        );
    }

    /// A core's page is built from what the core itself said, sorted into the
    /// groups the core itself named.
    ///
    /// The record is parsed from the helper's own wire format rather than
    /// built field by field, so this also says the two sides still agree about
    /// what a core's settings look like on the way over.
    fn ppsspp() -> crate::retroarch::CoreOptions {
        serde_json::from_str(
            r#"{
              "protocol": 1,
              "core": "ppsspp",
              "display": "PPSSPP",
              "categories": [
                {"key": "video", "title": "Video"},
                {"key": "system", "title": "System"},
                {"key": "empty", "title": "Nothing In Here"}
              ],
              "options": [
                {"key": "ppsspp_internal_resolution", "title": "Rendering Resolution",
                 "category": "video", "default": "480x272",
                 "values": [{"value": "480x272", "label": "1x (480x272)"},
                            {"value": "960x544", "label": "2x (960x544)"}]},
                {"key": "ppsspp_texture_scaling_level", "title": "Texture Upscaling Level",
                 "category": "video", "default": "disabled",
                 "values": [{"value": "disabled", "label": "disabled"},
                            {"value": "2x", "label": "2x"}]},
                {"key": "ppsspp_cpu_core", "title": "CPU Core",
                 "category": "system", "default": "JIT",
                 "values": [{"value": "JIT", "label": "Dynarec (JIT)"},
                            {"value": "Interpreter", "label": "Interpreter"}]},
                {"key": "ppsspp_loose", "title": "Sorted Nowhere",
                 "category": null, "default": "off",
                 "values": [{"value": "off", "label": "off"},
                            {"value": "on", "label": "on"}]}
              ]
            }"#,
        )
        .expect("the helper's own record")
    }

    #[test]
    fn a_cores_page_is_the_groups_that_core_named() {
        // Which console this emulator plays, as the scan would have said.
        // Stated before the page is drawn because the page reaches for it.
        let _held = crate::retroarch::GLOBALS
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        crate::retroarch::set_core_marks(vec![(
            "ppsspp".to_string(),
            "lxb:console-psp".to_string(),
        )]);
        let page = core_page(&ppsspp());
        assert_eq!(page.title(), "PPSSPP");
        assert_eq!(page.comment(), Some("4 settings"));
        // And it wears that console's mark rather than RetroArch's own. The
        // whole point of the row: four emulators under one page all wearing the
        // frontend's drawing are four rows told apart only by reading them.
        //
        // Guarded on the drawing having shipped, because the atlas a unit test
        // asks is this machine's — a package installed without its data
        // directory falls back, and that is correct rather than a failure.
        if crate::icons::shaped("lxb:console-psp") {
            assert_eq!(page.icon(), Some("lxb:console-psp"));
            assert_ne!(
                page.icon(),
                Some(crate::retroarch::mark()),
                "and not the frontend's"
            );
        }

        let rows = page.entries().expect("a core opens a column");
        let titles: Vec<&str> = rows.iter().map(Entry::title).collect();
        // The core's own order, the group it filled but named last left out,
        // and everything it sorted nowhere gathered at the end.
        assert_eq!(titles, vec!["Video", "System", "Other"]);

        let video = rows[0].entries().expect("a group opens a column");
        assert_eq!(
            video.iter().map(Entry::title).collect::<Vec<_>>(),
            vec!["Rendering Resolution", "Texture Upscaling Level"]
        );
    }

    /// An emulator that declares nothing and still wants a BIOS opens as a
    /// page with that one row on it, wearing its console's mark.
    ///
    /// LRPS2 exactly: it hands over no table until a game is loaded into it,
    /// and it cannot start one without a PlayStation 2 BIOS. Both halves are
    /// pinned here because this is the page somebody arrives at from a game
    /// that would not play.
    #[test]
    fn an_emulator_with_only_a_bios_is_a_page_wearing_its_console() {
        let _held = crate::retroarch::GLOBALS
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        crate::retroarch::set_core_marks(vec![(
            "pcsx2".to_string(),
            "lxb:console-ps2".to_string(),
        )]);
        crate::retroarch::set_firmware(vec![crate::retroarch::Firmware {
            console: "PlayStation 2".to_string(),
            core: "pcsx2".to_string(),
            note: "'pcsx2/bios' folder".to_string(),
            into: std::path::PathBuf::from("/system/pcsx2/bios"),
            here: false,
        }]);

        let core: crate::retroarch::CoreOptions = serde_json::from_str(
            r#"{"protocol":1,"core":"pcsx2","display":"LRPS2","categories":[],"options":[]}"#,
        )
        .expect("the helper's own wire format");
        let page = core_page(&core);
        assert_eq!(page.title(), "LRPS2");
        let rows = page.entries().expect("a page rather than a row");
        assert_eq!(
            rows.iter().map(Entry::title).collect::<Vec<_>>(),
            vec!["PlayStation 2 BIOS"],
            "the one thing on it"
        );
        if crate::icons::shaped("lxb:console-ps2") {
            assert_eq!(
                page.icon(),
                Some("lxb:console-ps2"),
                "and the page wears the console rather than the frontend"
            );
        }

        crate::retroarch::set_firmware(Vec::new());
        crate::retroarch::set_core_marks(Vec::new());
    }

    /// A core too old to sort its settings puts them straight on its page,
    /// rather than one folder called Other holding the whole emulator.
    #[test]
    fn a_core_that_sorts_nothing_has_a_flat_page() {
        let mut core = ppsspp();
        core.categories.clear();
        for option in &mut core.options {
            option.category = None;
        }
        let page = core_page(&core);
        let rows = page.entries().expect("a core opens a column");
        assert_eq!(rows.len(), 4, "every setting, and no folder around them");
        assert_eq!(rows[0].title(), "Rendering Resolution");
    }

    /// The row says what the setting is set to, and the value in force is the
    /// one wearing the tick.
    ///
    /// Nothing has been chosen here, so what it reads is the core's own
    /// default — which is what RetroArch itself does with a line that is not in
    /// the file.
    #[test]
    fn an_unset_option_reads_as_the_cores_own_default() {
        let core = ppsspp();
        let row = core_option_row(&core.display, &core.options[0]);
        assert_eq!(row.title(), "Rendering Resolution");
        assert_eq!(row.comment(), Some("1x (480x272)"));

        let values = row.entries().expect("a setting opens its values");
        assert_eq!(
            values.iter().map(Entry::title).collect::<Vec<_>>(),
            vec!["1x (480x272)", "2x (960x544)"]
        );
        assert!(values[0].chosen(), "the default is in force");
        assert!(!values[1].chosen());
        assert_eq!(
            values[1].setting(),
            Some(Setting::CoreOption {
                core: "PPSSPP",
                key: "ppsspp_internal_resolution",
                value: "960x544",
            }),
            "and choosing it writes that value under the name RetroArch files \
             this core's settings by"
        );
    }

    /// The settings that belong to no core come after the ones that do, and
    /// each says what is in force.
    #[test]
    fn the_settings_belonging_to_no_core_come_after_the_cores() {
        let page = retroarch();
        let rows = page.entries().expect("RetroArch opens a column");
        let titles: Vec<&str> = rows.iter().map(Entry::title).collect();
        // No core has answered in a test, so this is the whole page.
        assert_eq!(
            titles,
            vec![
                "Aspect ratio",
                "Video driver",
                "Whole-number scaling",
                "Wait for the screen",
                "ROMs path",
                "Get the artwork again"
            ]
        );

        let aspect = &rows[0];
        assert_eq!(
            aspect.comment(),
            Some("As the console had it"),
            "RetroArch's own default, which is what an unset machine uses"
        );
        let values = aspect.entries().expect("it opens its values");
        assert!(values[0].chosen(), "and it is the one ticked");
        assert_eq!(
            values[2].setting(),
            Some(Setting::Emulator {
                key: "aspect_ratio_index",
                value: "0",
            }),
            "4:3 is index 0 in RetroArch's own table"
        );
    }

    /// Games is a page of its own, in front of System, and Steam is what is on
    /// it.
    ///
    /// The place in the column is the half worth pinning down: Games is neither
    /// about what the machine talks to nor about the machine, and it sits
    /// between the two for that reason. The other half is that it is not a dead
    /// end — a subcategory the bar will not step into is a row that does
    /// nothing when pressed, see `Cursor::enter`, which refuses an empty column
    /// outright.
    #[test]
    fn the_games_page_sits_between_bluetooth_and_the_machine() {
        with_displays(&[], || {
            let column = column();
            let titles: Vec<&str> = column.iter().map(Entry::title).collect();
            let at = |title: &str| {
                titles
                    .iter()
                    .position(|had| *had == title)
                    .unwrap_or_else(|| panic!("the Settings column has a {title} row"))
            };
            assert!(
                at("Bluetooth") < at("Games") && at("Games") < at("System"),
                "the games sit between what the machine talks to and the machine: {titles:?}"
            );

            let row = &column[at("Games")];
            assert_eq!(row.icon(), Some(icons::CATEGORY_GAMES), "the pad");
            assert!(row.comment().is_some(), "and it says what is behind it");

            let page = row.entries().expect("Games opens a column");
            assert_eq!(page[0].title(), "Steam", "which every machine has");
            assert_eq!(page[0].icon(), Some(icons::STEAM));
            assert!(
                page[0].entries().is_some_and(|rows| !rows.is_empty()),
                "and it opens onto a column the bar can step into"
            );
        });
    }

    /// The row that stands in for an empty Games page still exists, and still
    /// says what it says.
    ///
    /// Unreachable now that Steam is on the page unconditionally, and worth a
    /// test all the same: what it guards is the rule, not the arrangement — a
    /// page of this tree with nothing on it is a row that does nothing when
    /// pressed, and the answer is words rather than emptiness.
    #[test]
    fn an_empty_games_page_says_so_rather_than_being_empty() {
        let waiting = nothing_to_set_about_games();
        assert_eq!(
            waiting.icon(),
            Some(icons::SETTING_INFO),
            "the shell's read-only mark"
        );
        assert!(waiting.comment().is_some(), "which says what is coming");
        assert_eq!(waiting.setting(), None, "there is nothing here to set");
        assert!(!waiting.chosen(), "and therefore no mark to carry");
        assert!(!waiting.starts_something(), "nor anything to start");
        assert!(waiting.entries().is_none(), "and no further column");
    }

    /// The Steam page under Games, as a session that has never been asked has
    /// it: the integration on, the client started for a game rather than with
    /// the shell, left running once that game is over, and one row that is not
    /// a switch at all.
    #[test]
    fn the_steam_page_offers_three_switches_and_says_what_they_are_set_to() {
        with_displays(&[], || {
            let rows = steam_page();
            assert_eq!(
                rows.iter().map(Entry::title).collect::<Vec<_>>(),
                [
                    "Integration",
                    "Start with the shell",
                    "Leave Steam running",
                    COMPATIBILITY_PAGE
                ],
                "the thing, starting it, stopping it, and what it runs games with"
            );
            let chosen = |row: &Entry| {
                row.entries()
                    .expect("each switch opens onto its two values")
                    .iter()
                    .position(Entry::chosen)
                    .expect("one of the two is in force")
            };
            assert_eq!(chosen(&rows[0]), 1, "the integration is on");
            assert_eq!(chosen(&rows[1]), 0, "the client waits for a game");
            assert_eq!(chosen(&rows[2]), 1, "and is left running after one");
            for row in &rows[..3] {
                let values = row.entries().expect("two values");
                assert_eq!(
                    values.iter().map(Entry::title).collect::<Vec<_>>(),
                    ["Off", "On"],
                    "the switch every other switch in this tree is"
                );
            }

            // And the fourth is not a switch: it is a list that has to be
            // fetched from Valve's client, so a session nobody has asked it in
            // shows the wait rather than an empty column somebody could step
            // into and find nothing in.
            let tools = rows[3].entries().expect("a column of tools");
            assert_eq!(tools.len(), 1, "one row, and it is the wait");
            assert_eq!(tools[0].setting(), None, "which cannot be pressed");
            assert_eq!(rows[3].icon(), Some(icons::SETTING_COMPATIBILITY));

            // The row above them says what is in here, not what it is set to:
            // three values read back would be a door labelled with the room's
            // furniture. See [`steam_note`].
            assert_eq!(
                steam_row().comment(),
                Some("How Steam works with the shell")
            );
        });
    }

    /// The two client settings are not offered by a shell that is not driving a
    /// client.
    ///
    /// A row that changes nothing is a row this tree does not offer — the same
    /// rule a screen list with no screens in it is under — and what says why is
    /// the one row that is left, which carries the answer under its own title.
    #[test]
    fn an_integration_that_is_off_offers_nothing_about_the_client() {
        with_displays(&[], || {
            assert!(apply_with(
                Setting::Steam(SteamValue::Integration(false)),
                |_| {},
            ));
            let rows = steam_page();
            assert_eq!(
                rows.iter().map(Entry::title).collect::<Vec<_>>(),
                ["Integration"],
                "and nothing about a client this shell is not starting"
            );
            assert_eq!(
                steam_row().comment(),
                Some("How Steam works with the shell"),
                "the door says what is behind it whichever way the switch is set"
            );
            // And the two readers say so whatever the file holds, which is what
            // keeps every caller of them from having to ask twice.
            *STEAM_AT_STARTUP.lock().unwrap() = true;
            *STEAM_AFTER_A_GAME.lock().unwrap() = false;
            assert!(!steam_at_startup(), "there is no client to start");
            assert!(
                steam_left_after_a_game(),
                "and none of this shell's business to close"
            );
        });
    }

    /// A session started with `--no-steam` has no switch to press, because the
    /// flag outranks the file: it says what *this session* was told to do, and
    /// a row saying On over a session doing nothing would be the page lying.
    ///
    /// What is written down is still the setting and never the flag. A machine
    /// booted once with `--no-steam` must not come back the next morning with
    /// its Steam integration quietly turned off.
    #[test]
    fn a_session_told_to_leave_steam_alone_says_so() {
        with_displays(&[], || {
            note_steam_in_this_session(false);
            assert!(!steam_integration(), "whatever the file says");

            let rows = steam_page();
            assert_eq!(rows.len(), 1, "one row, and it is not a switch");
            assert_eq!(rows[0].icon(), Some(icons::SETTING_INFO));
            assert_eq!(rows[0].setting(), None, "there is nothing here to set");
            assert!(rows[0].comment().is_some(), "which says why");
            assert_eq!(steam_row().comment(), Some("Left out of this session"));

            assert_eq!(
                stored().steam_integration,
                Some(true),
                "the flag is not an answer the user gave"
            );
        });
    }

    /// The three switches survive being written out and read back, which is the
    /// whole point of writing them down.
    ///
    /// Never through [`apply`], for the reason the volume test gives: that one
    /// writes to the config directory of whoever is running the tests.
    #[test]
    fn the_steam_switches_are_remembered() {
        with_displays(&[], || {
            let mut persisted = None;
            assert!(apply_with(
                Setting::Steam(SteamValue::AtStartup(true)),
                |stored| persisted = stored.steam_at_startup,
            ));
            assert!(steam_at_startup());
            assert_eq!(persisted, Some(true), "and it is written down");
            assert!(apply_with(
                Setting::Steam(SteamValue::AfterAGame(false)),
                |_| {},
            ));
            assert!(!steam_left_after_a_game());

            let body = toml::to_string_pretty(&stored()).unwrap();
            assert!(body.contains("steam-at-startup"), "{body}");
            assert!(body.contains("steam-after-a-game"), "{body}");
            *STEAM_AT_STARTUP.lock().unwrap() = false;
            *STEAM_AFTER_A_GAME.lock().unwrap() = true;
            adopt(toml::from_str(&body).unwrap());
            assert!(steam_at_startup(), "and they come back as they were");
            assert!(!steam_left_after_a_game());

            // A file that says nothing about them changes nothing, which is how
            // every settings file written before this page existed keeps the
            // Steam this shell has always had. See [`adopt`].
            assert_eq!(Stored::default().steam_integration, None);
            adopt(Stored::default());
            assert!(steam_at_startup(), "a silent file answers nothing");
            assert!(!steam_left_after_a_game());
            assert!(steam_integration(), "and the default is on");
        });
    }

    /// Highlighting a Steam row changes nothing.
    ///
    /// It matters most for the integration, which is the one setting in this
    /// tree whose press rebuilds the bar: a cursor walking past Off would take
    /// somebody's library off the screen and put it back, twice, on the way
    /// down a list of two.
    #[test]
    fn a_highlighted_steam_row_is_not_a_chosen_one() {
        with_displays(&[], || {
            preview(Some(Setting::Steam(SteamValue::Integration(false))));
            assert!(steam_integration(), "highlighting Off is not choosing it");
            preview(Some(Setting::Steam(SteamValue::AtStartup(true))));
            assert!(!steam_at_startup());
        });
    }

    /// The login screen is told when its half of these settings changes, and
    /// left alone when the rest of them do.
    ///
    /// Both halves of that matter. A user who changes their accent and signs
    /// straight out has to meet the colour they chose, which is why this is
    /// said here rather than watched for from outside; and a user holding the
    /// volume down has to not start a process per step, which is why it is not
    /// said every time the file is written.
    #[test]
    fn the_login_screen_hears_about_colours_and_screens_and_nothing_else() {
        let _guard = LOCK.lock().unwrap_or_else(|held| held.into_inner());
        *SHOWN.lock().unwrap() = None;

        let mut stored = Stored {
            accent: Some("Purple".to_string()),
            ..Stored::default()
        };
        // Nothing has been told yet, so the first save is news whatever it says.
        assert!(news_for_the_login_screen(&stored));
        assert!(!news_for_the_login_screen(&stored));

        stored.accent = Some("Red".to_string());
        assert!(news_for_the_login_screen(&stored));
        assert!(!news_for_the_login_screen(&stored));

        // The volume, on every step of a held direction, which is what this
        // guard exists for.
        for step in 0..10 {
            stored.sound_volume = Some(step as f32 / 10.0);
            assert!(
                !news_for_the_login_screen(&stored),
                "the login screen was told about the volume"
            );
        }
        stored.do_not_disturb = Some(true);
        stored.media_sort.insert("Music".into(), "name".into());
        stored.steam_sort = Some("name".into());
        assert!(!news_for_the_login_screen(&stored));

        // A screen turned to HDR is news, and so is one plugged in for the
        // first time: the login screen brings both of them up.
        stored.display.insert(
            "TEST-OUT-1".to_string(),
            StoredDisplay {
                hdr: Some(true),
                ..StoredDisplay::default()
            },
        );
        assert!(news_for_the_login_screen(&stored));
        assert!(!news_for_the_login_screen(&stored));

        stored
            .display
            .get_mut("TEST-OUT-1")
            .expect("the display just added")
            .hdr_sdr_brightness = Some(250);
        assert!(news_for_the_login_screen(&stored));

        // And what every display with no section of its own comes up in.
        stored.hdr = Some(true);
        assert!(news_for_the_login_screen(&stored));
        assert!(!news_for_the_login_screen(&stored));

        *SHOWN.lock().unwrap() = None;
    }

    /// A machine with no such login screen is not kept waiting on its way out.
    ///
    /// The publish spent at Exit and Shut down waits for its child, which every
    /// other one deliberately does not: a session that exits takes its children
    /// with it, so not waiting would mean the news never arrives. That wait must
    /// cost nothing at all where there is nobody to tell — this shell does not
    /// require that display manager to be installed, and a second and a half
    /// added to every logout on every machine that runs something else would be
    /// a poor way to find that out.
    #[test]
    fn a_machine_with_another_login_screen_is_not_kept_waiting_to_leave() {
        let started = Instant::now();
        assert!(start_the_login_screen(&["lxb-no-such-login-screen"]).is_none());
        // Whatever the far end is, the *absence* of one is answered by two
        // failed lookups and nothing else.
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "a machine with no login screen of ours paid for the wait anyway"
        );
    }

    /// A name this machine does not have is the next name, not the end of the
    /// list.
    ///
    /// The login screen was renamed, and this shell went on asking for the name
    /// it used to install under. Nothing said so — a name nothing answers to is
    /// what a machine with somebody else's login screen looks like from here —
    /// and the accent quietly stopped reaching it: the only copy the greeter
    /// ever saw was the one published at sign-in, so a colour chosen and then
    /// signed out of took a whole second session to appear.
    ///
    /// `true` stands in for the greeter here. What is being checked is the walk
    /// down the list, which is the part that has to survive the next rename;
    /// whether the real name is spelled right is a question about another
    /// project's packaging and no test here can answer it.
    #[test]
    fn a_login_screen_under_an_older_name_is_still_told() {
        assert_eq!(
            tell_the_login_screen(&["lxb-no-such-login-screen", "true"]).as_deref(),
            Some("true"),
            "a name this machine does not have ended the search"
        );
        assert_eq!(
            tell_the_login_screen(&["lxb-no-such-login-screen"]),
            None,
            "something was started for a machine with no login screen of ours"
        );
        // The newest name is asked for first, so a machine with both installed
        // runs the one that is current rather than one left behind by an
        // upgrade.
        assert_eq!(LOGIN_SCREENS.first(), Some(&"cedm"));
    }

    // -----------------------------------------------------------------------
    // network
    // -----------------------------------------------------------------------

    /// Everything below is invented hardware. Nothing this machine is on may
    /// appear here: an interface name or a network taken off a live scan is a
    /// test that passes or fails by which desk it was run at.
    const SOCKET: &str = "/an/invented/socket";
    const RADIO: &str = "/an/invented/radio";

    fn a_socket() -> crate::network::Device {
        crate::network::Device {
            path: SOCKET.to_string(),
            interface: "test-wired0".to_string(),
            kind: crate::network::Kind::Wired,
            link: crate::network::Link::Up,
            trouble: None,
            connection: Some("A wired profile".to_string()),
            address: Some("10.0.0.2/24".to_string()),
            gateway: Some("10.0.0.1".to_string()),
            nameservers: vec!["10.0.0.1".to_string(), "10.0.0.9".to_string()],
            hardware: Some("00:00:5e:00:53:01".to_string()),
            carrier: Some(true),
            speed: 1000,
            profile: Some("/an/invented/profile".to_string()),
            ipv4: crate::network::Ipv4 {
                automatic: true,
                address: None,
                gateway: None,
                dns_automatic: true,
                dns: Vec::new(),
                can_pin: true,
            },
        }
    }

    fn a_radio() -> crate::network::Device {
        crate::network::Device {
            path: RADIO.to_string(),
            interface: "test-wireless0".to_string(),
            kind: crate::network::Kind::Wireless,
            connection: Some("Upstairs".to_string()),
            carrier: None,
            speed: 300,
            ..a_socket()
        }
    }

    fn a_network(
        ssid: &str,
        strength: u8,
        security: crate::network::Security,
        saved: bool,
        joined: bool,
    ) -> crate::network::Network {
        crate::network::Network {
            ssid: ssid.to_string(),
            strength,
            security,
            saved,
            joined,
            frequency: 5180,
        }
    }

    /// A listing with whichever halves the test is about, and nothing else.
    fn reported(
        devices: Vec<crate::network::Device>,
        networks: Vec<(String, Vec<crate::network::Network>)>,
    ) -> crate::network::Listing {
        crate::network::Listing {
            manager: true,
            radio: true,
            radio_switchable: true,
            devices,
            networks,
            wanted: None,
        }
    }

    /// The Network column, wherever it has got to in the tree.
    fn network_page() -> Vec<Entry> {
        let column = column();
        let row = column
            .iter()
            .find(|entry| entry.title() == "Network")
            .expect("Settings has a Network row");
        row.entries().expect("Network opens a column").to_vec()
    }

    fn titles(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(Entry::title).collect()
    }

    fn under<'a>(entries: &'a [Entry], title: &str) -> &'a [Entry] {
        row(entries, title)
            .entries()
            .unwrap_or_else(|| panic!("{title} opens a column"))
    }

    /// One row of a column, by the name on it.
    fn row<'a>(entries: &'a [Entry], title: &str) -> &'a Entry {
        entries
            .iter()
            .find(|entry| entry.title() == title)
            .unwrap_or_else(|| panic!("{title} is not on this page: {:?}", titles(entries)))
    }

    // --- Settings > Input ---------------------------------------------------

    // --- Settings > Input > Keyboard ----------------------------------------

    /// The Keyboard layout page, with the session told it can set one.
    ///
    /// Both halves are needed: the page is gated on the protocol version — see
    /// [`KEYBOARD_LAYOUT_AVAILABLE`] — and every test here is about what it
    /// offers when it is offered at all.
    fn layout_page() -> Vec<Entry> {
        note_keyboard_layout_available(true);
        under(
            under(under(&column(), "Input"), "Keyboard"),
            "Keyboard layout",
        )
        .to_vec()
    }

    /// Whether this machine has a layout registry to read, which a build
    /// container may not. See [[tests-that-read-the-machine]].
    fn has_layouts() -> bool {
        !layouts::registry().is_empty()
    }

    /// Three levels, with the field at the head of every one of them.
    ///
    /// The field is `over_the_list`, which is what keeps the column opening on
    /// the first continent rather than on a control nobody asked for — the
    /// same rule the shelves and the explorer's folders are under.
    #[test]
    fn the_layout_tree_is_continents_then_countries_then_arrangements() {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());
        let saved = take_settings();
        if has_layouts() {
            let page = layout_page();
            assert!(page[0].over_the_list(), "the field stands over the list");
            assert_eq!(page[0].title(), "Search");
            assert!(
                !page[1].over_the_list(),
                "and nothing under it does, so the column opens on a continent"
            );

            // Every row below the field opens a column of countries, and every
            // one of those opens a column of arrangements.
            let continents = &page[1..];
            assert!(!continents.is_empty());
            for continent in continents {
                let countries = continent.entries().expect("a continent opens a column");
                assert_eq!(countries[0].title(), "Search", "{}", continent.title());
                assert!(countries.len() > 1, "{} is empty", continent.title());
                for country in &countries[1..] {
                    let held = country.entries().expect("a country opens a column");
                    assert_eq!(held[0].title(), "Search", "{}", country.title());
                    assert!(held.len() > 1, "{} is empty", country.title());
                    for arrangement in &held[1..] {
                        assert!(
                            arrangement.setting().is_some(),
                            "{} sets nothing",
                            arrangement.title()
                        );
                    }
                }
            }
        }
        put_back(saved);
    }

    /// A press writes the arrangement down as `layout (variant)`, and the row
    /// above the page says what it is by name.
    #[test]
    fn choosing_an_arrangement_is_written_down_and_said_on_the_row_above_it() {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());
        let saved = take_settings();
        if has_layouts() {
            note_keyboard_layout_available(true);
            // Named exactly, not "the first thing that matches polish" — that
            // is `gb (pl)`, Polish on a British keyboard, and a test that
            // asserted on whichever row xkeyboard-config happened to list first
            // would be a test about the package rather than about this page.
            let wanted = layouts::registry()
                .find("pl", "")
                .map(|layout| (layout.key(), layout.name.clone()));
            if let Some((wanted, called)) = wanted {
                let mut persisted = None;
                assert!(apply_with(
                    Setting::KeyboardLayout(intern(&wanted)),
                    |stored| persisted = stored.keyboard_layout.clone(),
                ));
                assert_eq!(persisted.as_deref(), Some(wanted.as_str()));
                assert_eq!(keyboard_layout().as_deref(), Some(wanted.as_str()));

                let opened = column();
                let keyboard = under(under(&opened, "Input"), "Keyboard");
                assert_eq!(
                    row(keyboard, "Keyboard layout").comment(),
                    Some(called.as_str()),
                    "the row carrying the value says what the value is"
                );
                // And the mark is on that row and on no other.
                let page = layout_page();
                let ticked: Vec<&str> = walk(&page)
                    .into_iter()
                    .filter(|entry| entry.chosen())
                    .map(|entry| entry.title())
                    .collect();
                assert_eq!(ticked, [called.as_str()]);
            }
        }
        put_back(saved);
    }

    /// Every row of the tree, however deep, for counting what is marked.
    fn walk(entries: &[Entry]) -> Vec<&Entry> {
        let mut found = Vec::new();
        for entry in entries {
            found.push(entry);
            if let Some(inside) = entry.entries() {
                found.extend(walk(inside));
            }
        }
        found
    }

    /// The field searches every arrangement from wherever it is typed into, so
    /// the same query answers the same way at all three levels — and the rows
    /// it finds say where they live instead of what xkb calls them.
    #[test]
    fn the_field_finds_arrangements_from_any_level_of_the_tree() {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());
        let saved = take_settings();
        if has_layouts() && !layouts::registry().search("polish").is_empty() {
            note_keyboard_layout_available(true);
            assert!(set_layout_query("polish"));
            let page = layout_page();
            assert_eq!(page[0].title(), "polish", "the field is the query");
            assert_eq!(
                page[1].title(),
                "Clear search",
                "and the row that empties it arrives with it"
            );
            let found: Vec<&str> = page[2..].iter().map(|entry| entry.title()).collect();
            assert!(found.contains(&"Polish"), "{found:?}");
            assert_eq!(
                row(&page, "Polish").comment(),
                Some("Europe · Poland"),
                "a found row says where it lives, not what xkb calls it"
            );

            // Every found row is an arrangement to press rather than a way
            // further in: a search has taken the user out of the tree.
            for entry in &page[2..] {
                assert!(entry.entries().is_none(), "{}", entry.title());
                assert!(entry.setting().is_some(), "{}", entry.title());
            }

            // And emptying it puts the tree back.
            assert!(set_layout_query(""));
            let page = layout_page();
            assert_eq!(page[0].title(), "Search");
            assert!(page.iter().all(|entry| entry.title() != "Polish"));
        }
        put_back(saved);
    }

    /// What is in force is the shell's answer, or the compositor's where the
    /// shell has never been asked — which is every machine the first time.
    #[test]
    fn the_compositor_s_own_layout_is_what_is_in_force_until_somebody_chooses() {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());
        let saved = take_settings();
        *KEYBOARD_LAYOUT.lock().unwrap() = None;
        *COMPOSITOR_LAYOUT.lock().unwrap() = None;
        assert_eq!(keyboard_layout_in_force(), None);

        assert!(note_compositor_layout("de".to_string()));
        assert_eq!(keyboard_layout_in_force().as_deref(), Some("de"));
        // Said twice is not news.
        assert!(!note_compositor_layout("de".to_string()));

        // Once the shell has one of its own it wins, and the compositor
        // reporting it back is not a change the column has to be rebuilt for.
        *KEYBOARD_LAYOUT.lock().unwrap() = Some("pl (qwertz)".to_string());
        assert_eq!(keyboard_layout_in_force().as_deref(), Some("pl (qwertz)"));
        assert!(!note_compositor_layout("pl (qwertz)".to_string()));

        // And what is written down is the shell's own and never the
        // compositor's: writing that one down would turn a setting nobody made
        // into one they did, after which changing config.toml would stop
        // working.
        *KEYBOARD_LAYOUT.lock().unwrap() = None;
        assert_eq!(stored().keyboard_layout, None);
        put_back(saved);
    }

    /// A compositor too old to be told says so rather than offering six hundred
    /// rows that would change nothing.
    #[test]
    fn a_session_that_cannot_set_a_layout_says_so_instead_of_listing_them() {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());
        let saved = take_settings();
        note_keyboard_layout_available(false);

        let opened = column();
        let keyboard = under(under(&opened, "Input"), "Keyboard");
        let row = row(keyboard, "Keyboard layout");
        assert_eq!(row.comment(), Some("Not offered by this session"));
        let page = row.entries().expect("it still opens a column");
        assert_eq!(titles(page), ["Not offered by this session"]);
        assert!(
            page[0].setting().is_none(),
            "and the one row on it cannot be pressed"
        );
        put_back(saved);
    }

    /// The board is *inside* the keyboard's page, and under the layout that
    /// decides what it prints.
    ///
    /// The two were siblings under Input for one release. They are not two
    /// subjects: the board is a keyboard — the one a console has instead of the
    /// one on the desk — and it takes its caps from the layout set beside it,
    /// so listing them at the same depth put a cause and its effect side by
    /// side with nothing saying which was which.
    #[test]
    fn the_board_lives_under_the_keyboard_whose_letters_it_prints() {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());
        let saved = take_settings();
        let opened = column();
        assert_eq!(titles(under(&opened, "Input")), ["Keyboard", "Mouse"]);
        assert_eq!(
            titles(under(under(&opened, "Input"), "Keyboard")),
            ["Keyboard layout", "On-screen keyboard"],
            "the page that decides comes before the page that follows it"
        );
        put_back(saved);
    }

    /// The Input column, and the keyboard's page inside it.
    fn keyboard_page() -> Vec<Entry> {
        under(
            under(under(&column(), "Input"), "Keyboard"),
            "On-screen keyboard",
        )
        .to_vec()
    }

    /// The page lists the screens this session has, headed by the answer that
    /// is not a screen at all.
    ///
    /// "Focused screen" is first because it is the setting's own default and
    /// because it is the only row on the page that is a *rule* rather than a
    /// place — everything under it names one screen, and a rule read after four
    /// connector names would be read as a fifth one.
    #[test]
    fn the_board_can_be_pinned_to_any_screen_the_session_has() {
        with_displays(
            &[(FIRST, Support::default()), (SECOND, Support::default())],
            || {
                let page = keyboard_page();
                assert_eq!(titles(&page), ["Default display"]);

                let rows = under(&page, "Default display");
                assert_eq!(
                    titles(rows),
                    [
                        FOCUSED_SCREEN,
                        &format!("Only {FIRST}"),
                        &format!("Only {SECOND}")
                    ]
                );
                assert!(
                    rows[0].chosen(),
                    "a shell nobody has asked follows the hands"
                );
                assert_eq!(rows[0].setting(), Some(Setting::KeyboardDisplay(None)));
                assert_eq!(
                    rows[2].setting(),
                    Some(Setting::KeyboardDisplay(Some(intern(SECOND))))
                );

                // Every row a screen is drawn as one, which is what the Display
                // pages draw the same screens as.
                for row in &rows[1..] {
                    assert_eq!(row.icon(), Some(icons::SETTING_DISPLAY));
                }
                // And nothing says anything: a connector name is what the rest
                // of this tree calls the same screen, and a sentence under each
                // explaining what it is would be the cable described back.
                assert!(rows[1..].iter().all(|row| row.comment().is_none()));
            },
        );
    }

    /// A press moves the mark, writes the name down, and is said back by the
    /// row above the list.
    ///
    /// That row and no other. The page above it describes the *page* — see
    /// [`on_screen_keyboard`], where that is argued — so a comment naming one
    /// screen must not climb any further than the setting it belongs to.
    #[test]
    fn pinning_the_board_is_written_down_and_said_on_the_row_above_it() {
        with_displays(
            &[(FIRST, Support::default()), (SECOND, Support::default())],
            || {
                let opened = column();
                let keyboard = under(under(&opened, "Input"), "Keyboard");
                let page = under(keyboard, "On-screen keyboard");
                assert_eq!(
                    row(keyboard, "On-screen keyboard").comment(),
                    Some("The board this shell types with"),
                    "the row that opens the page describes the page"
                );
                assert_eq!(page[0].comment(), Some(FOCUSED_SCREEN));

                let rows = under(&keyboard_page(), "Default display").to_vec();
                let mut persisted = None;
                // Never through `apply`, which would write to the config
                // directory of whoever is running the suite.
                assert!(apply_with(
                    rows[1].setting().expect("the row sets something"),
                    |stored| persisted = stored.keyboard_display.clone(),
                ));
                assert_eq!(persisted.as_deref(), Some(FIRST));
                assert_eq!(keyboard_display().as_deref(), Some(FIRST));

                let opened = column();
                let keyboard = under(under(&opened, "Input"), "Keyboard");
                let page = under(keyboard, "On-screen keyboard");
                let said = format!("Only {FIRST}");
                assert_eq!(
                    row(keyboard, "On-screen keyboard").comment(),
                    Some("The board this shell types with"),
                    "and it goes on describing the page, whatever is chosen inside it"
                );
                assert_eq!(page[0].comment(), Some(said.as_str()));
                let rows = under(page, "Default display");
                assert_eq!(
                    rows.iter()
                        .filter(|row| row.chosen())
                        .map(Entry::title)
                        .collect::<Vec<_>>(),
                    [said.as_str()]
                );

                // Highlighting one changes nothing. It is the row here where a
                // preview would be visible and still wrong: the board is what
                // the user is reading the list through, and walking the list
                // would throw it from screen to screen under their hands.
                preview(rows[0].setting());
                assert_eq!(keyboard_display().as_deref(), Some(FIRST));
            },
        );
    }

    /// A screen the setting names and this session has not got is still
    /// offered, still marked, and says why it is not there.
    ///
    /// The setting is still in force — the board goes back to that screen the
    /// moment it is plugged in — so a page with nothing ticked on it would be
    /// the shell denying a choice it is keeping. The same answer the startup
    /// column gives for a column that is not on the bar.
    #[test]
    fn a_screen_that_is_not_plugged_in_is_still_the_setting() {
        with_displays(&[(FIRST, Support::default())], || {
            *KEYBOARD_DISPLAY.lock().unwrap() = Some(AWKWARD.to_string());

            let page = keyboard_page();
            let rows = under(&page, "Default display");
            let absent = format!("Only {AWKWARD}");
            assert_eq!(
                titles(rows),
                [FOCUSED_SCREEN, &format!("Only {FIRST}"), &absent],
                "the screen it names is offered last, after the ones that are here"
            );
            assert!(rows[2].chosen(), "and it is still what is chosen");
            assert_eq!(rows[2].comment(), Some("Not plugged in just now"));
            assert!(
                rows[1].comment().is_none(),
                "a screen that is here says nothing"
            );
            assert_eq!(
                page[0].comment(),
                Some(format!("Only {AWKWARD} — not plugged in just now").as_str())
            );

            // And it is not written back to anything else, which is what brings
            // the board home when the cable goes back in.
            assert_eq!(stored().keyboard_display.as_deref(), Some(AWKWARD));
        });
    }

    /// The Mouse page, wherever it has got to in the tree.
    fn mouse_page() -> Vec<Entry> {
        under(under(&column(), "Input"), "Mouse").to_vec()
    }

    /// The four rows, in the order the page keeps them, each carrying its own
    /// kind of row: a bar for the two speeds and a list for the other two.
    #[test]
    fn the_mouse_page_sets_the_pointer_and_the_wheel() {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());
        let saved = take_settings();

        let page = mouse_page();
        assert_eq!(
            titles(&page),
            [
                "Cursor speed",
                "Cursor size",
                "Scrolling speed",
                "Scrolling direction"
            ],
            "each pair's speed in front of the other thing about it"
        );

        // The two speeds are bars: one row that is the number, walked either
        // way along its own track.
        for (row, foot, head) in [
            (
                "Cursor speed",
                Setting::Pointer(PointerValue::Speed(SLOWEST_POINTER)),
                Setting::Pointer(PointerValue::Speed(FASTEST_POINTER)),
            ),
            (
                "Scrolling speed",
                Setting::Pointer(PointerValue::Scroll(SLOWEST_SCROLL)),
                Setting::Pointer(PointerValue::Scroll(FASTEST_SCROLL)),
            ),
        ] {
            let rows = under(&page, row);
            let [Entry::Bar(bar)] = rows else {
                panic!("{row} is one bar and nothing else: {:?}", titles(rows));
            };
            assert_eq!(bar.steps.first(), Some(&foot));
            assert_eq!(bar.steps.last(), Some(&head));
            // The handle starts in the middle of the pointer's track — 0 is
            // libinput's flat default, not its slow end — and at the foot of
            // nothing: a scroll speed of one to one is a quarter of the way up.
            assert!(
                (0.0..=1.0).contains(&bar.fill),
                "{row} puts its handle on its own track"
            );
        }

        // The cursor's size is a list, because a theme draws a handful of sizes
        // and the ones in between would move the number and not the pointer.
        let sizes = under(&page, "Cursor size");
        assert_eq!(titles(sizes), ["Small", "Normal", "Large", "Larger"]);
        assert_eq!(
            sizes
                .iter()
                .filter(|row| row.chosen())
                .map(Entry::title)
                .collect::<Vec<_>>(),
            ["Normal"],
            "a session nobody has asked draws the cursor at XCursor's own size"
        );
        assert_eq!(
            sizes[0].setting(),
            Some(Setting::Pointer(PointerValue::Size(16)))
        );

        // And the direction is two rows named after what moves, because
        // "natural" is the one word that tells nobody anything.
        let direction = under(&page, "Scrolling direction");
        assert_eq!(titles(direction), ["Standard", "Natural"]);
        assert!(direction[0].chosen(), "the wheel's own direction, to begin");
        assert_eq!(
            direction[1].setting(),
            Some(Setting::Pointer(PointerValue::Natural(true)))
        );

        put_back(saved);
    }

    /// A press changes one of the four and leaves the other three alone, and
    /// all four are written down.
    ///
    /// One at a time matters here more than on most pages: what goes over the
    /// wire is all four together — see `Shell::sync_pointer` — so a press that
    /// carried a stale copy of its neighbours would undo them.
    #[test]
    fn one_mouse_row_is_set_without_moving_the_others() {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());
        let saved = take_settings();

        assert_eq!(pointer(), Pointer::DEFAULT);

        let mut persisted = None;
        // Never through `apply`, which would write to the config directory of
        // whoever is running the suite.
        assert!(apply_with(
            Setting::Pointer(PointerValue::Scroll(200)),
            |stored| persisted = Some(stored.scroll_speed),
        ));
        assert_eq!(persisted, Some(Some(200)));
        assert_eq!(
            pointer(),
            Pointer {
                scroll: 200,
                ..Pointer::DEFAULT
            },
            "the wheel moved and the pointer did not"
        );

        assert!(apply_with(
            Setting::Pointer(PointerValue::Natural(true)),
            |_| {}
        ));
        assert!(apply_with(Setting::Pointer(PointerValue::Size(48)), |_| {}));
        assert!(apply_with(
            Setting::Pointer(PointerValue::Speed(-40)),
            |_| {}
        ));
        assert_eq!(
            pointer(),
            Pointer {
                speed: -40,
                size: 48,
                scroll: 200,
                natural: true
            }
        );

        // The whole of it is written, whichever row was pressed.
        let written = stored();
        assert_eq!(written.pointer_speed, Some(-40));
        assert_eq!(written.cursor_size, Some(48));
        assert_eq!(written.scroll_speed, Some(200));
        assert_eq!(written.natural_scroll, Some(true));

        // And the page says so on the rows above the values.
        let page = mouse_page();
        assert_eq!(
            page.iter().map(Entry::comment).collect::<Vec<_>>(),
            [
                Some("30%"),
                Some("Larger"),
                Some("200%"),
                Some("The page follows your fingers")
            ]
        );

        // Highlighting one changes nothing. The pointer is what the user is
        // reading the list *with*, so a preview would change how fast the walk
        // itself moves under the hand doing the walking.
        preview(under(&page, "Cursor size")[0].setting());
        assert_eq!(pointer().size, 48);

        put_back(saved);
    }

    /// A number the page cannot ask for is brought to the nearest one that
    /// means something, on the way in and on the way out.
    ///
    /// The file is one the user is entitled to open, and so is a file written
    /// by a later version of this shell offering sizes or speeds this one does
    /// not. Neither is refused: refusing would leave the page with nothing
    /// ticked on it and no explanation, which is the answer the startup column
    /// and the keyboard's display both decline to give.
    #[test]
    fn a_mouse_setting_out_of_range_is_brought_to_the_nearest_one() {
        // The cursor's sizes are a list, so "nearest" is one of the four.
        assert_eq!(nearest_cursor_size(0), 16);
        assert_eq!(nearest_cursor_size(20), 16, "a tie goes to the first");
        assert_eq!(nearest_cursor_size(21), NATURAL_CURSOR);
        assert_eq!(nearest_cursor_size(40), 32, "and so does the one above");
        assert_eq!(nearest_cursor_size(41), 48);
        assert_eq!(nearest_cursor_size(4096), 48);

        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());
        let saved = take_settings();

        // The two ranges are clamped, which is what a hand-edited number gets.
        assert!(apply_with(
            Setting::Pointer(PointerValue::Speed(i8::MIN)),
            |_| {}
        ));
        assert_eq!(pointer().speed, SLOWEST_POINTER);
        assert!(apply_with(
            Setting::Pointer(PointerValue::Scroll(u16::MAX)),
            |_| {}
        ));
        assert_eq!(pointer().scroll, FASTEST_SCROLL);

        put_back(saved);
    }

    // --- Settings > Users ---------------------------------------------------

    fn account(uid: u64, name: &str, admin: bool) -> crate::users::Person {
        crate::users::Person {
            uid,
            path: format!("/org/freedesktop/Accounts/User{uid}"),
            name: name.to_string(),
            real: String::new(),
            admin,
            picture: None,
            stamp: None,
            home: PathBuf::from(format!("/home/{name}")),
            here: false,
            you: false,
        }
    }

    fn accounts(people: Vec<crate::users::Person>) -> crate::users::Listing {
        crate::users::Listing {
            daemon: true,
            people,
            trouble: None,
            working: false,
        }
    }

    /// The page, built against a machine the test describes.
    ///
    /// The listing goes through [`crate::users::note`], which is the same door
    /// the worker's own answers come through — so what is being tested is the
    /// page the shell would really draw and not a second way of building one.
    fn users_page(listing: crate::users::Listing) -> Vec<Entry> {
        crate::users::note(listing);
        let Entry::Folder(page) = users() else {
            panic!("Users is a subcategory");
        };
        page.entries
    }

    /// Every account is listed, the row that makes another is at the foot, and
    /// each account's row wears its own picture rather than a mark.
    ///
    /// The mark a picture-less account falls back to is the *single* figure and
    /// never the two the page itself is reached by — see
    /// [`icons::SETTING_USERS`]. A page whose every row wore the heading above
    /// it would say only "a user" on a page where every row is one.
    #[test]
    fn every_account_is_listed_with_its_own_face_and_a_way_to_add_another() {
        let _alone = crate::users::one_form_at_a_time();
        let mut marta = account(1000, "marta", true);
        marta.real = "Marta Kowalska".to_string();
        // A picture the shell can really see, so the row carries it: the check
        // that the file exists is the worker's, and this page draws what it is
        // handed.
        let face = std::env::temp_dir().join("lxb-users-test-face.png");
        std::fs::write(&face, b"not really a picture, but a file").expect("a scratch file");
        marta.picture = Some(face.clone());

        let page = users_page(accounts(vec![marta, account(1001, "jan", false)]));
        assert_eq!(titles(&page), ["Marta Kowalska", "jan", "Add user"]);

        let Entry::Folder(first) = &page[0] else {
            panic!("an account opens a column");
        };
        assert_eq!(first.portrait.as_deref(), Some(face.as_path()));
        assert_eq!(first.comment.as_deref(), Some("marta — Administrator"));
        assert_eq!(
            first.icon.as_deref(),
            Some(icons::SETTING_PERSON),
            "the mark under a face is the single figure"
        );
        assert_eq!(first.person, Some(crate::users::Whose::Existing(1000)));

        let Entry::Folder(second) = &page[1] else {
            panic!("an account opens a column");
        };
        assert_eq!(second.portrait, None, "no picture, so it wears the figure");
        assert_eq!(second.icon.as_deref(), Some(icons::SETTING_PERSON));

        let Entry::Folder(add) = &page[2] else {
            panic!("Add user opens a column");
        };
        assert_eq!(add.icon.as_deref(), Some(icons::SETTING_ADD_USER));
        assert_eq!(add.person, Some(crate::users::Whose::New));

        // And the page itself is the two figures, which is the one mark on it
        // that says "a set of people" rather than "a person".
        let Entry::Folder(page) = users() else {
            panic!("Users is a subcategory");
        };
        assert_eq!(page.icon.as_deref(), Some(icons::SETTING_USERS));
        assert_ne!(
            icons::SETTING_USERS,
            icons::SETTING_PERSON,
            "a face's fallback must not be the mark of the page it is on"
        );
    }

    /// An account somebody is using does not offer to rename itself.
    ///
    /// `usermod -l` refuses for anything with a process running, so there is
    /// nothing behind the row: it says the name and says why, rather than
    /// opening a keyboard to take a value it will refuse a column later. The
    /// row stays — what somebody logs in as is worth reading on a page about
    /// them — and the two sentences differ, because for your own account the
    /// thing that has to happen next is yours to do.
    #[test]
    fn a_signed_in_account_is_not_offered_a_new_user_name() {
        let _alone = crate::users::one_form_at_a_time();
        let mine = crate::users::Person {
            here: true,
            you: true,
            ..account(1000, "marta", true)
        };
        let theirs = crate::users::Person {
            here: true,
            ..account(1001, "jan", true)
        };
        let away = account(1002, "ola", false);
        let page = users_page(accounts(vec![mine, theirs, away]));

        let ours = &under(&page, "marta")[1];
        assert_eq!(ours.title(), "Username");
        assert!(ours.typed().is_none(), "nothing to type into");
        // Still the field's own mark. A row that cannot be pressed is not an
        // explanation, and this form has been a stack of identical marks once.
        assert_eq!(ours.icon(), Some(icons::SETTING_USERNAME));
        assert!(ours
            .comment()
            .is_some_and(|note| note.starts_with("marta —") && note.contains("you are signed in")));

        let theirs = &under(&page, "jan")[1];
        assert!(theirs.typed().is_none());
        assert!(
            theirs
                .comment()
                .is_some_and(|note| note.contains("they are signed in")),
            "somebody else signing out is not something you do"
        );

        // And an account nobody is using is a field like any other.
        let away = &under(&page, "ola")[1];
        assert_eq!(away.title(), "Username");
        assert!(away.typed().is_some(), "this one can still be renamed");
        assert_eq!(away.comment(), Some("ola"));
    }

    /// Both forms carry the same rows, in the same order — and the one an
    /// account already has opens holding what that account is.
    #[test]
    fn both_forms_ask_the_same_things_in_the_same_order() {
        let _alone = crate::users::one_form_at_a_time();
        let mut marta = account(1000, "marta", true);
        marta.real = "Marta Kowalska".to_string();
        let page = users_page(accounts(vec![marta, account(1001, "jan", true)]));

        let new = under(&page, "Add user");
        assert_eq!(
            titles(new),
            [
                "Name",
                "Username",
                "Account type",
                "Password",
                "Confirm password",
                "Avatar",
                "Accept and create",
            ]
        );
        // Nothing is set on a form for an account that does not exist.
        assert_eq!(new[0].comment(), Some("Not set"));
        assert_eq!(new[3].comment(), Some("Not set"), "no password yet");
        assert_eq!(under(new, "Account type")[0].title(), "Standard");
        assert!(
            under(new, "Account type")[0].chosen(),
            "Standard by default"
        );

        // An account that already exists is not asked twice — see
        // [`crate::users::asks_twice`]. Everything else is the same form.
        let existing = under(&page, "Marta Kowalska");
        assert_eq!(
            titles(existing),
            [
                "Name",
                "Username",
                "Account type",
                "Password",
                "Avatar",
                "Save changes",
                "Remove account",
            ]
        );
        assert_eq!(existing[0].comment(), Some("Marta Kowalska"));
        assert_eq!(existing[1].comment(), Some("marta"));
        // An account that already has a password says the field would leave it
        // alone, which is what an empty one means there.
        assert_eq!(existing[3].comment(), Some("Unchanged"));
        // And each row wears a mark of its own. Three rows sharing one drawing
        // is a form nobody can read at a glance, which is what these were.
        let marks: Vec<Option<&str>> = existing.iter().map(Entry::icon).collect();
        assert_eq!(
            marks,
            [
                Some(icons::SETTING_NAME),
                Some(icons::SETTING_USERNAME),
                Some(icons::SETTING_ACCOUNT_TYPE),
                Some(icons::SETTING_PASSWORD),
                Some(icons::SETTING_AVATAR),
                // The shell's one tick, which a picker's answer row wears
                // too: both are pressed to commit. Not the padlock it wore
                // first — a padlock is the thing that is locked, and this row
                // is agreeing to get past one.
                Some(icons::CHOSEN),
                // The bin, because this machine has a second administrator on
                // it and Marta can therefore be removed.
                Some(icons::UNINSTALL),
            ]
        );
        assert_ne!(
            icons::SETTING_PASSWORD,
            icons::AUTHENTICATE,
            "the key and the padlock are two moments, not one"
        );
        assert!(
            under(existing, "Account type")[1].chosen(),
            "Marta is an administrator"
        );
    }

    /// The last administrator cannot be made a standard account, and cannot be
    /// removed — and both say why on the row rather than by not being there.
    ///
    /// This is the whole of the guard the page carries. Either press would leave
    /// a machine nobody can administer, and nothing in this shell could put that
    /// right afterwards: every way back needs an administrator to agree to it.
    #[test]
    fn the_last_administrator_cannot_be_demoted_or_removed() {
        let _alone = crate::users::one_form_at_a_time();
        let alone = users_page(accounts(vec![
            account(1000, "marta", true),
            account(1001, "jan", false),
        ]));
        let marta = under(&alone, "marta");
        // The row is there, and it is a line rather than a way further in.
        let kind = marta
            .iter()
            .find(|row| row.title() == "Account type")
            .expect("the row is still on the page");
        assert!(kind.entries().is_none(), "it does not open a column");
        assert!(kind.setting().is_none(), "and it cannot be pressed");
        assert!(kind.comment().unwrap_or_default().contains("only one"));

        let removal = marta
            .iter()
            .find(|row| row.title() == "Remove account")
            .expect("the row is still on the page");
        assert!(removal.setting().is_none(), "it cannot be pressed");

        // A standard account beside them has both.
        let jan = under(&alone, "jan");
        assert!(under(jan, "Account type").len() == 2);
        assert_eq!(
            under(&alone, "jan")
                .iter()
                .find(|row| row.title() == "Remove account")
                .and_then(Entry::setting),
            Some(Setting::User(UserValue::Remove(1001)))
        );

        // With a second administrator on the machine, Marta gets both back.
        let shared = users_page(accounts(vec![
            account(1000, "marta", true),
            account(1001, "jan", true),
        ]));
        let marta = under(&shared, "marta");
        assert_eq!(under(marta, "Account type").len(), 2);
        assert_eq!(
            marta
                .iter()
                .find(|row| row.title() == "Remove account")
                .and_then(Entry::setting),
            Some(Setting::User(UserValue::Remove(1000)))
        );
    }

    /// Somebody signed in cannot be removed, and neither can the account the
    /// session is running as — each saying which of the two it is.
    #[test]
    fn an_account_in_use_cannot_be_removed() {
        let _alone = crate::users::one_form_at_a_time();
        let mut here = account(1001, "jan", false);
        here.here = true;
        let mut yours = account(1002, "ola", false);
        yours.here = true;
        yours.you = true;

        let page = users_page(accounts(vec![
            account(1000, "marta", true),
            here,
            yours,
            account(1003, "piotr", false),
        ]));

        let removal = |name: &str| {
            let rows = under(&page, name);
            let row = rows
                .iter()
                .find(|row| row.title() == "Remove account")
                .expect("every account has the row");
            (row.setting(), row.comment().unwrap_or_default().to_string())
        };

        let (setting, why) = removal("jan");
        assert!(setting.is_none(), "signed in");
        assert!(why.contains("signed in"), "{why}");

        let (setting, why) = removal("ola");
        assert!(setting.is_none(), "this session's own account");
        assert!(why.contains("this session"), "{why}");

        // And one who is neither can be.
        let (setting, _) = removal("piotr");
        assert_eq!(setting, Some(Setting::User(UserValue::Remove(1003))));
        // The row that says so is an *action*: it does a thing rather than
        // answering the question its column asks, so it never takes the mark.
        let rows = under(&page, "piotr");
        let row = rows
            .iter()
            .find(|row| row.title() == "Remove account")
            .expect("the row");
        assert!(row.acts(), "a removal is not one of a set of answers");
        assert!(!row.chosen());
    }

    /// The row at the foot of a form cannot be pressed until the form describes
    /// an account, and says what is missing while it does not.
    #[test]
    fn the_accept_row_says_what_is_missing_until_there_is_nothing() {
        let _alone = crate::users::one_form_at_a_time();
        let page = users_page(accounts(vec![account(1000, "marta", true)]));
        let new = under(&page, "Add user");
        let accept = new.last().expect("the last row on the form");
        assert_eq!(accept.title(), "Accept and create");
        assert!(
            accept.setting().is_none(),
            "an empty form cannot be handed over"
        );
        assert!(
            accept.comment().unwrap_or_default().contains("user name"),
            "it says what is missing: {:?}",
            accept.comment()
        );
    }

    /// While a change is in flight the whole form stops offering to make it
    /// again — which on this page nearly always means polkit is waiting for
    /// somebody to type a password.
    #[test]
    fn nothing_can_be_pressed_twice_while_polkit_is_asking() {
        let _alone = crate::users::one_form_at_a_time();
        let page = users_page(crate::users::Listing {
            working: true,
            ..accounts(vec![account(1000, "marta", true)])
        });
        let accept = under(&page, "Add user").last().cloned().expect("a row");
        assert!(accept.setting().is_none());
        assert!(
            accept.comment().unwrap_or_default().contains("permission"),
            "{:?}",
            accept.comment()
        );
    }

    /// A machine with no account service says so in words, in a column that can
    /// still be stepped into — the same bargain the Network page states.
    #[test]
    fn a_machine_with_no_account_service_says_so() {
        let _alone = crate::users::one_form_at_a_time();
        let page = users_page(crate::users::Listing::default());
        assert_eq!(page.len(), 1, "one row, not an empty column");
        assert!(page[0].title().contains("No account service"));
        assert!(page[0].setting().is_none(), "and it cannot be pressed");
    }

    /// What went wrong with the last press is said at the head of the page.
    ///
    /// At the head because it is about the page rather than about one account,
    /// and because a refusal below the fold would be a press that looked as
    /// though it had done nothing.
    #[test]
    fn a_refused_change_is_reported_at_the_head_of_the_page() {
        let _alone = crate::users::one_form_at_a_time();
        let page = users_page(crate::users::Listing {
            trouble: Some(crate::users::Trouble {
                what: "The account could not be created".to_string(),
                why: "Not authorized.".to_string(),
            }),
            ..accounts(vec![account(1000, "marta", true)])
        });
        // Both halves, and this way round: the row is titled with what the
        // shell was doing and noted with what the machine said about it. It
        // read "That did not work" over the shell's own half once, and a page
        // that never showed the machine's half is how a refused password went
        // a morning without an explanation.
        assert_eq!(page[0].title(), "The account could not be created");
        assert_eq!(page[0].comment(), Some("Not authorized."));
        assert_eq!(titles(&page)[1..], ["marta", "Add user"]);
    }

    /// The avatar is chosen by walking the disk, and the walk lists only what a
    /// face can be.
    #[test]
    fn an_avatar_is_chosen_by_walking_to_it() {
        let _alone = crate::users::one_form_at_a_time();
        let page = users_page(accounts(vec![account(1000, "marta", true)]));
        let rows = under(&page, "marta");
        let avatar = rows
            .iter()
            .find(|row| row.title() == "Avatar")
            .expect("the row");
        let Entry::Folder(folder) = avatar else {
            panic!("Avatar opens a column");
        };
        assert_eq!(
            folder.place.as_ref().map(crate::files::Place::shows),
            Some(crate::files::Shows::Portrait)
        );
        assert_eq!(folder.comment.as_deref(), Some("None — the plain mark"));
        // Taking one off is not a row on the form. It is an answer to the
        // question the picker asks, and it lives in there.
        assert!(!titles(rows).iter().any(|title| title.contains("no avatar")));
    }

    /// Taking the avatar off is the other answer at the head of the picker —
    /// offered only where there is one to take off, and never the row the
    /// column opens on.
    #[test]
    fn no_avatar_is_an_answer_inside_the_picker_rather_than_a_row_beside_it() {
        let _alone = crate::users::one_form_at_a_time();
        let marta = account(1000, "marta", true);
        crate::users::note(accounts(vec![marta.clone()]));

        // A form with no avatar has nothing to offer.
        crate::users::open_form(crate::users::Whose::Existing(1000), Some(&marta));
        assert!(no_avatar_row().is_none());

        // One with an avatar does, and that row stands over the list — so the
        // column still opens on the first disk and a press of A on the way in
        // cannot clear somebody's avatar before they have read it.
        crate::users::set_picture(Path::new("/tmp/face.png"));
        let row = no_avatar_row().expect("there is an avatar to take off");
        assert_eq!(row.title(), "Use no avatar");
        assert!(
            row.over_the_list(),
            "it must not be what the column opens on"
        );
        assert!(
            row.acts(),
            "it does a thing rather than answering the column"
        );
        assert!(!row.chosen());
        assert_eq!(row.setting(), Some(Setting::User(UserValue::DropPicture)));

        // And with no form open at all there is nothing to say.
        crate::users::close_form();
        assert!(no_avatar_row().is_none());
    }

    /// A password typed into an account that already exists is enough on its
    /// own: the row says how many characters are waiting, and the row at the
    /// foot lets the change through.
    ///
    /// This is the bug the Confirm rule was written for. The form used to carry
    /// a Confirm row on *both* kinds of account and check the two against each
    /// other whatever the form was — so setting a password on somebody who
    /// already had one left the second field empty, and Save was refused with
    /// "The two passwords are not the same" for a field that was not on the
    /// page. The account could not be changed at all.
    #[test]
    fn a_password_on_an_existing_account_needs_no_second_field() {
        let _alone = crate::users::one_form_at_a_time();
        let marta = account(1000, "marta", true);
        crate::users::note(accounts(vec![marta.clone(), account(1001, "jan", true)]));
        crate::users::open_form(crate::users::Whose::Existing(1000), Some(&marta));

        let mut typed = crate::secret::Secret::default();
        for character in "hunter2".chars() {
            typed.push(character);
        }
        crate::users::write_secret(crate::users::Field::Password, typed);

        let page = users_page(accounts(vec![marta, account(1001, "jan", true)]));
        let rows = under(&page, "marta");
        // The row says what is waiting rather than going on saying "Unchanged",
        // which is what it said before anything was typed.
        assert_eq!(
            rows.iter()
                .find(|row| row.title() == "Password")
                .and_then(Entry::comment),
            Some("7 characters")
        );
        // And the change goes through.
        let save = rows
            .iter()
            .find(|row| row.title() == "Save changes")
            .expect("the row at the foot");
        assert_eq!(
            save.setting(),
            Some(Setting::User(UserValue::Accept)),
            "refused with: {:?}",
            save.comment()
        );
        crate::users::close_form();
    }

    /// A new account still is asked twice, and is refused until the two agree —
    /// which is what a confirmation is for: nobody can get into that account
    /// yet, so a typo is only discovered at the login screen.
    #[test]
    fn a_new_account_is_asked_twice_and_refused_until_they_agree() {
        let _alone = crate::users::one_form_at_a_time();
        crate::users::note(accounts(vec![account(1000, "marta", true)]));
        crate::users::open_form(crate::users::Whose::New, None);
        crate::users::write(crate::users::Field::Username, "jan");

        let secret = |text: &str| {
            let mut secret = crate::secret::Secret::default();
            for character in text.chars() {
                secret.push(character);
            }
            secret
        };
        crate::users::write_secret(crate::users::Field::Password, secret("hunter2"));

        let accept = |page: &[Entry]| {
            under(page, "Add user")
                .iter()
                .find(|row| row.title() == "Accept and create")
                .expect("the row at the foot")
                .clone()
        };

        let page = users_page(accounts(vec![account(1000, "marta", true)]));
        assert!(
            accept(&page).setting().is_none(),
            "one password and no confirmation"
        );

        crate::users::write_secret(crate::users::Field::Confirm, secret("hunter3"));
        let page = users_page(accounts(vec![account(1000, "marta", true)]));
        assert!(accept(&page).setting().is_none(), "they do not agree");

        crate::users::write_secret(crate::users::Field::Confirm, secret("hunter2"));
        let page = users_page(accounts(vec![account(1000, "marta", true)]));
        assert_eq!(
            accept(&page).setting(),
            Some(Setting::User(UserValue::Accept)),
            "refused with: {:?}",
            accept(&page).comment()
        );
        crate::users::close_form();
    }

    /// Users sits between Games and System, which is where somebody looking for
    /// it comes to rest — see [`column`], where the whole order is argued.
    #[test]
    fn users_stands_between_the_games_and_the_machine() {
        let column = column();
        let at = |title: &str| {
            column
                .iter()
                .position(|entry| entry.title() == title)
                .unwrap_or_else(|| panic!("{title} is not in the Settings column"))
        };
        assert!(at("Games") < at("Users") && at("Users") < at("System"));
    }

    /// The whole of what the user asked for, in one test: a machine with both
    /// halves shows both, and a machine with one shows one.
    ///
    /// The Wi-Fi page is the one that must not be there. A radio page on a
    /// desktop with no wireless card is not merely useless — it is a page that
    /// has the user turning a radio on and off looking for a network that was
    /// never going to appear, and then wondering what is wrong with the shell.
    #[test]
    fn the_network_column_lists_only_what_the_machine_has() {
        with_network(reported(vec![a_socket(), a_radio()], Vec::new()), || {
            assert_eq!(
                titles(&network_page()),
                ["Wired", "Wi-Fi"],
                "the socket is the plainer thing and comes first"
            );
        });
        with_network(reported(vec![a_socket()], Vec::new()), || {
            assert_eq!(titles(&network_page()), ["Wired"]);
        });
        with_network(reported(vec![a_radio()], Vec::new()), || {
            assert_eq!(titles(&network_page()), ["Wi-Fi"]);
        });
    }

    /// A machine with nothing to configure still has a row, and it says which
    /// of the two reasons it is: the hardware, or the session.
    #[test]
    fn a_machine_with_no_network_says_which_kind_of_nothing_it_is() {
        for (listing, expected) in [
            (reported(Vec::new(), Vec::new()), "No network hardware"),
            (
                crate::network::Listing::none(),
                "No network manager is running",
            ),
        ] {
            let manager = listing.manager;
            with_network(listing, || {
                let page = network_page();
                assert_eq!(titles(&page), [expected], "manager: {manager}");
                // Never empty — the bar refuses to step into a column with
                // nothing in it — and never pressable, because a row that
                // describes something true must not be one the user can
                // un-choose.
                assert_eq!(page.len(), 1);
                assert!(page[0].setting().is_none());
                assert!(page[0].comment().is_some(), "it has to say why");
            });
        }
    }

    /// The radio switch is the whole of the Wi-Fi page while the radio is off:
    /// a card that cannot hear anything has nothing to list, and a Networks row
    /// standing over an empty list would be a way in with nothing behind it.
    #[test]
    fn the_wi_fi_page_is_the_switch_alone_while_the_radio_is_off() {
        let mut listing = reported(vec![a_radio()], Vec::new());
        listing.radio = false;
        with_network(listing, || {
            let wifi = under(&network_page(), "Wi-Fi").to_vec();
            assert_eq!(titles(&wifi), ["Wi-Fi"]);
            let switch = under(&wifi, "Wi-Fi");
            assert_eq!(titles(switch), ["Off", "On"], "Off is above On, as ever");
            assert!(switch[0].chosen(), "the radio is off and the mark says so");
            assert_eq!(
                switch[1].setting(),
                Some(Setting::Network(NetworkValue::Radio(true)))
            );
        });
    }

    /// A radio the machine has switched off in hardware is not offered a switch
    /// at all: On would be accepted, nothing would happen, and the mark would
    /// come to rest on a row describing a machine that is not this one.
    #[test]
    fn a_radio_killed_in_hardware_is_explained_rather_than_offered() {
        let mut listing = reported(vec![a_radio()], Vec::new());
        listing.radio = false;
        listing.radio_switchable = false;
        with_network(listing, || {
            let wifi = under(&network_page(), "Wi-Fi").to_vec();
            assert_eq!(titles(&wifi), ["Wi-Fi is off at the machine"]);
            assert!(wifi[0].setting().is_none());
        });

        // And the same page when the software switch reads *on* over a hardware
        // one that is off, which is a state `NetworkManager` really reports: no
        // networks are going to arrive, so there is no page of them to offer.
        let mut listing = reported(vec![a_radio()], Vec::new());
        listing.radio_switchable = false;
        with_network(listing, || {
            assert_eq!(
                titles(under(&network_page(), "Wi-Fi")),
                ["Wi-Fi is off at the machine"]
            );
        });
    }

    /// The list of networks: what is in the air, and nothing else.
    ///
    /// It used to open with a `Not connected` row. The way off a network is
    /// inside the network now, which is where somebody looking for it looks,
    /// and a row above the list doing the same thing at a distance was one row
    /// of ceremony on every visit for a press most people make once.
    #[test]
    fn the_networks_page_is_the_air_and_nothing_else() {
        use crate::network::Security;
        let networks = vec![
            a_network("Upstairs", 62, Security::Personal, true, true),
            a_network("The Cafe", 91, Security::Open, false, false),
            a_network("The Office", 40, Security::Enterprise, false, false),
            a_network("Next Door", 30, Security::Modern, false, false),
        ];
        with_network(
            reported(vec![a_radio()], vec![(RADIO.to_string(), networks)]),
            || {
                let wifi = under(&network_page(), "Wi-Fi").to_vec();
                assert_eq!(
                    titles(&wifi),
                    ["Wi-Fi", "Networks", "Connection information"],
                    "the addressing is not here: it belongs to the network"
                );

                let page = under(&wifi, "Networks").to_vec();
                assert_eq!(
                    titles(&page),
                    ["Upstairs", "The Cafe", "The Office", "Next Door"],
                    "the one in force first, then the rest by how well they are \
                     heard"
                );

                // Exactly one of them is in force, and it is the one that is
                // joined.
                assert_eq!(page.iter().filter(|entry| entry.chosen()).count(), 1);
                assert!(page[0].chosen());
                // And it is the one row here that is stepped into rather than
                // pressed: the radio is already on it, so joining it again is
                // the one press with nothing to do, and behind it is what only
                // that network has — the addressing of the profile it is on,
                // and the way off it.
                assert_eq!(page[0].setting(), None);
                assert_eq!(
                    titles(page[0].entries().expect("the joined network opens")),
                    ["IP address", "DNS", "Disconnect", "Forget"]
                );
                assert_eq!(
                    page[1].setting(),
                    Some(Setting::Network(NetworkValue::Join {
                        device: intern(RADIO),
                        ssid: intern("The Cafe"),
                    })),
                    "a network with nothing saved for it is joined by pressing it"
                );

                // What each row says under its name: what joining it takes,
                // and how well it is heard.
                assert_eq!(page[0].comment(), Some("Connected — 62% at 5 GHz"));
                assert_eq!(page[1].comment(), Some("Open — 91% at 5 GHz"));
                assert_eq!(page[3].comment(), Some("WPA3 — 30% at 5 GHz"));

                // The one row here that can be read and not pressed. It is
                // listed so that "my network is not here" has an answer, and it
                // carries no setting because a password would not get it on.
                assert_eq!(page[2].title(), "The Office");
                assert!(page[2].setting().is_none());
                assert!(page[2]
                    .comment()
                    .is_some_and(|note| note.contains("certificate")));
            },
        );
    }

    /// A radio that hears nothing gets a line saying so, not an empty column.
    ///
    /// `Not connected` used to guarantee this column a row. With it gone, a
    /// radio that is on and hearing nothing would leave `Networks` opening onto
    /// nothing at all — and [`crate::model::Cursor::enter`] will not open an
    /// empty column, so the press would do nothing whatever, which reads as a
    /// shell that failed rather than as an answer.
    #[test]
    fn a_radio_that_hears_nothing_says_so_rather_than_opening_on_nothing() {
        with_network(
            reported(vec![a_radio()], vec![(RADIO.to_string(), Vec::new())]),
            || {
                let wifi = under(&network_page(), "Wi-Fi").to_vec();
                let row = wifi
                    .iter()
                    .find(|entry| entry.title() == "Networks")
                    .expect("the Networks row is there whatever is in the air");
                assert_eq!(row.comment(), Some("Nothing in range"));

                let page = row.entries().expect("and it still opens");
                assert_eq!(titles(page), ["Nothing in range"]);
                assert!(
                    page[0].setting().is_none(),
                    "there is nothing here to press"
                );
                assert!(!page[0].chosen(), "and nothing to be the answer");
            },
        );
    }

    /// A network the machine remembers is stepped into rather than joined where
    /// it stands, and behind it are the two things there are to do with one.
    ///
    /// The press it costs is the point of the test as much as the rows are. A
    /// remembered network is the only kind that has more than one thing to do,
    /// and forgetting has nowhere else in the shell it could live: the list is
    /// otherwise a list of what is in the *air*, and what the machine remembers
    /// shows there only as a word in a comment.
    #[test]
    fn a_remembered_network_is_stepped_into_rather_than_joined() {
        use crate::network::Security;
        with_network(
            reported(
                vec![crate::network::Device {
                    link: crate::network::Link::Idle,
                    connection: None,
                    ..a_radio()
                }],
                vec![(
                    RADIO.to_string(),
                    vec![
                        a_network("Known", 55, Security::Personal, true, false),
                        a_network("Unknown", 55, Security::Personal, false, false),
                    ],
                )],
            ),
            || {
                let page = under(under(&network_page(), "Wi-Fi"), "Networks").to_vec();
                assert_eq!(titles(&page), ["Known", "Unknown"]);

                // Remembered: a way in, and no press of its own. Joining is
                // now one of the things inside rather than the whole row.
                assert_eq!(page[0].setting(), None);
                let known = page[0].entries().expect("a remembered network opens");
                assert_eq!(titles(known), ["Connect", "Forget"]);
                assert_eq!(
                    known[0].setting(),
                    Some(Setting::Network(NetworkValue::Join {
                        device: intern(RADIO),
                        ssid: intern("Known"),
                    }))
                );
                assert_eq!(
                    known[1].setting(),
                    Some(Setting::Network(NetworkValue::Forget {
                        device: intern(RADIO),
                        ssid: intern("Known"),
                    }))
                );

                // Never marked, either of them. The mark on this page says
                // which network the radio is on, and nothing on it is marked
                // at all while it is on none — the question has no answer yet
                // rather than an answer called none.
                assert!(known.iter().all(|row| !row.chosen()));
                assert!(known.iter().all(Entry::acts));
                assert_eq!(page.iter().filter(|row| row.chosen()).count(), 0);

                // A network the machine has never been on keeps its single
                // press: there is nothing saved to remove, and nothing to
                // configure until it has been joined once.
                assert!(page[1].entries().is_none());
                assert_eq!(
                    page[1].setting(),
                    Some(Setting::Network(NetworkValue::Join {
                        device: intern(RADIO),
                        ssid: intern("Unknown"),
                    }))
                );
            },
        );
    }

    /// The network the radio is on carries the same pair, with the way off it
    /// in place of the way on to it — and both of them under the addressing,
    /// which is what a column opening on its first row lands on.
    #[test]
    fn the_network_in_force_offers_the_way_off_it_and_the_way_to_lose_it() {
        use crate::network::Security;
        with_network(
            reported(
                vec![a_radio()],
                vec![(
                    RADIO.to_string(),
                    vec![a_network("Upstairs", 62, Security::Personal, true, true)],
                )],
            ),
            || {
                let page = under(under(&network_page(), "Wi-Fi"), "Networks").to_vec();
                let joined = page[0].entries().expect("the joined network opens");
                assert_eq!(
                    titles(joined),
                    ["IP address", "DNS", "Disconnect", "Forget"]
                );
                assert!(
                    !joined[0].acts() && !joined[1].acts(),
                    "the first row a column opens on must not be one that does \
                     something"
                );
                assert!(joined[2].acts() && joined[3].acts());

                // The only way off a network in the shell: the column this row
                // stands in is the air and nothing else, so there is nowhere
                // else this could be.
                assert_eq!(
                    joined[2].setting(),
                    Some(Setting::Network(NetworkValue::Leave {
                        device: intern(RADIO)
                    }))
                );
                assert_eq!(
                    joined[3].setting(),
                    Some(Setting::Network(NetworkValue::Forget {
                        device: intern(RADIO),
                        ssid: intern("Upstairs"),
                    }))
                );

                // The tick is still the folder's own and nothing inside it has
                // taken one.
                assert!(page[0].chosen());
                assert!(joined.iter().all(|row| !row.chosen()));

                // And what Forget costs is on the row, because there is no
                // panel between the press and the act.
                assert!(joined[3]
                    .comment()
                    .is_some_and(|note| note.contains("password will be asked for again")));
            },
        );
    }

    /// A saved network says so rather than saying what protects it: those are
    /// the same fact from either side, and only one of them is what the user is
    /// about to find out — a saved network joins on the press, an unsaved
    /// secured one asks for a password first.
    #[test]
    fn a_saved_network_says_it_will_not_ask() {
        use crate::network::Security;
        with_network(
            reported(
                vec![a_radio()],
                vec![(
                    RADIO.to_string(),
                    vec![
                        a_network("Known", 55, Security::Personal, true, false),
                        a_network("Unknown", 55, Security::Personal, false, false),
                    ],
                )],
            ),
            || {
                let page = under(under(&network_page(), "Wi-Fi"), "Networks").to_vec();
                assert_eq!(page[0].comment(), Some("Saved — 55% at 5 GHz"));
                assert_eq!(page[1].comment(), Some("WPA2 — 55% at 5 GHz"));
            },
        );
    }

    /// The wired page, and the one thing on it that is not a switch: a socket
    /// with nothing plugged into it. There is nothing for On to do there, and a
    /// row that took the press and left the mark on Off would be the page
    /// arguing with something the user can see from where they are sitting.
    #[test]
    fn an_empty_socket_is_explained_rather_than_switched() {
        with_network(reported(vec![a_socket()], Vec::new()), || {
            let wired = under(&network_page(), "Wired").to_vec();
            assert_eq!(
                titles(&wired),
                ["Connection", "IP address", "DNS", "Connection information"]
            );
            let switch = under(&wired, "Connection");
            assert_eq!(titles(switch), ["Off", "On"]);
            assert!(switch[1].chosen(), "it is up, and the mark says so");
            assert_eq!(
                switch[0].setting(),
                Some(Setting::Network(NetworkValue::Wire {
                    device: intern(SOCKET),
                    up: false
                }))
            );
        });

        let empty = crate::network::Device {
            link: crate::network::Link::Unavailable,
            trouble: Some("No cable".to_string()),
            connection: None,
            address: None,
            gateway: None,
            nameservers: Vec::new(),
            carrier: Some(false),
            speed: 0,
            ..a_socket()
        };
        with_network(reported(vec![empty], Vec::new()), || {
            let wired = under(&network_page(), "Wired").to_vec();
            assert_eq!(
                titles(&wired),
                ["No cable", "IP address", "DNS"],
                "a socket with no cable in it still has a profile to configure"
            );
            assert!(wired[0].setting().is_none());
        });
    }

    /// What a device was given is a panel behind a row, and only when there is
    /// something to report.
    ///
    /// A panel and not a column, for the reason System information is one: a
    /// column is walked down one row at a time and only the row the cursor is
    /// on says its value, so a list of five facts would be five presses to
    /// read — in a shell where walking down a list is how a value is *chosen*.
    /// And a page of settings whose controls are outnumbered by its footnotes
    /// is a page the user has to read past to find out there is nothing more to
    /// change.
    #[test]
    fn what_a_device_was_given_is_a_panel_to_read() {
        with_network(reported(vec![a_socket()], Vec::new()), || {
            let page = network_page();
            let row = under(&page, "Wired")
                .iter()
                .find(|entry| entry.title() == "Connection information")
                .expect("a socket with an address says what it was given")
                .clone();
            // Nothing about it is a row of the bar: no setting to choose, no
            // mark to carry, and no column to walk into.
            assert_eq!(row.setting(), None);
            assert!(!row.chosen());
            assert!(row.entries().is_none());
            assert!(!row.starts_something());

            let Some(crate::apps::About::Listed(values)) =
                row.facts().map(|facts| facts.about.clone())
            else {
                panic!("the values travel with the row rather than being read on the press");
            };
            assert_eq!(
                values
                    .iter()
                    .map(|(label, _)| label.as_str())
                    .collect::<Vec<_>>(),
                [
                    "IP address",
                    "Router",
                    "DNS",
                    "Interface",
                    "Hardware address",
                    "Link speed"
                ]
            );
            assert_eq!(values[0].1, "10.0.0.2/24");
            // Every name server on one line: a machine is given two or three
            // and they are one answer rather than three things to be read one
            // at a time.
            assert_eq!(values[2].1, "10.0.0.1, 10.0.0.9");
        });

        // Nothing to report is no row at all, rather than a page of blanks.
        let idle = crate::network::Device {
            link: crate::network::Link::Idle,
            trouble: None,
            connection: None,
            address: None,
            gateway: None,
            nameservers: Vec::new(),
            hardware: None,
            speed: 0,
            ..a_socket()
        };
        with_network(reported(vec![idle], Vec::new()), || {
            assert_eq!(
                titles(under(&network_page(), "Wired")),
                ["Connection", "IP address", "DNS"],
                "a device with no address still has addressing to set"
            );
        });
    }

    /// A machine with two of one kind gets a page per device, named by the
    /// interface — because "connect" is not a question a laptop in a dock has
    /// one answer to.
    #[test]
    fn several_of_one_kind_get_a_page_each() {
        let second = crate::network::Device {
            path: "/another/invented/socket".to_string(),
            interface: "test-wired1".to_string(),
            ..a_socket()
        };
        with_network(reported(vec![a_socket(), second], Vec::new()), || {
            let wired = under(&network_page(), "Wired").to_vec();
            assert_eq!(titles(&wired), ["test-wired0", "test-wired1"]);
            for socket in &wired {
                assert_eq!(
                    titles(socket.entries().expect("a socket opens its controls"))[0],
                    "Connection"
                );
            }
        });
    }

    /// The two rows at the heads of these pages say what is *inside* them, the
    /// way every other subcategory in this tree does — not what the machine
    /// happens to be doing at the moment they were drawn.
    ///
    /// They both used to say the second thing, and it was wrong twice over. It
    /// made these the only two rows in the Settings column that answered a
    /// question instead of describing a page, so a user reading down the column
    /// met four descriptions and two status lines. And it meant a row's own
    /// comment changed under a cursor standing on it: "Connected to Upstairs"
    /// one second and "Connecting…" the next is not a description of anything,
    /// it is a status line that has wandered into a menu.
    ///
    /// What the machine is doing has not been lost — it is on the rows inside,
    /// where it is about one socket or one device and can be acted on.
    #[test]
    fn the_two_connection_rows_say_what_is_inside_them() {
        let note = |title: &str| {
            column()
                .iter()
                .find(|entry| entry.title() == title)
                .and_then(Entry::comment)
                .map(str::to_string)
                .unwrap_or_else(|| panic!("the {title} row says something"))
        };

        // Whatever the machine is doing, and whether or not there is anything
        // to do it with.
        for listing in [
            reported(vec![a_socket(), a_radio()], Vec::new()),
            crate::network::Listing::none(),
        ] {
            with_network(listing, || {
                assert_eq!(
                    note("Network"),
                    "The socket in the back of the machine, and the air around it"
                );
            });
        }
        for listing in [
            one_controller(vec![a_device(
                EARS,
                "Ears",
                crate::bluetooth::Kind::Headphones,
                true,
                true,
            )]),
            crate::bluetooth::Listing::none(),
        ] {
            with_bluetooth(listing, || {
                assert_eq!(
                    note("Bluetooth"),
                    "The devices this machine pairs with, and the controller it pairs from"
                );
            });
        }

        // And they read like the four that were always right.
        for (title, note) in [
            ("Appearance", "How the shell looks"),
            ("Display", "How the picture reaches the screen"),
            ("Sounds", "The machine's sound, and the shell's own"),
            ("System", "How the machine behaves"),
        ] {
            assert_eq!(
                column()
                    .iter()
                    .find(|entry| entry.title() == title)
                    .and_then(Entry::comment),
                Some(note)
            );
        }
    }

    /// The addressing page, in the two shapes it has.
    ///
    /// The values Manual needs are inside its own column rather than beside it,
    /// which is what keeps the device page two rows longer instead of five and
    /// keeps a row named `Address` from standing under one named `IP address`.
    #[test]
    fn addressing_is_automatic_or_pinned() {
        let automatic = a_socket();
        with_network(reported(vec![automatic], Vec::new()), || {
            let page = network_page();
            let addressing = under(under(&page, "Wired"), "IP address").to_vec();
            assert_eq!(
                titles(&addressing),
                ["Automatic", "Manual"],
                "nothing to type until there is something to type it into"
            );
            assert!(addressing[0].chosen());
            assert_eq!(
                addressing[1].setting(),
                Some(Setting::Network(NetworkValue::Addressing {
                    device: intern(SOCKET),
                    automatic: false
                }))
            );
        });

        let pinned = crate::network::Device {
            ipv4: crate::network::Ipv4 {
                automatic: false,
                address: Some("10.0.0.2/24".to_string()),
                gateway: Some("10.0.0.1".to_string()),
                dns_automatic: false,
                dns: vec!["9.9.9.9".to_string(), "1.1.1.1".to_string()],
                can_pin: true,
            },
            ..a_socket()
        };
        with_network(reported(vec![pinned], Vec::new()), || {
            let page = network_page();
            let wired = under(&page, "Wired");
            let addressing = under(wired, "IP address").to_vec();
            assert_eq!(
                titles(&addressing),
                ["Automatic", "Manual", "Address", "Router"]
            );
            assert!(addressing[1].chosen());

            // The two values are typed, not chosen: no setting to apply and no
            // mark to carry, because nothing here is one of a set.
            for row in &addressing[2..] {
                assert_eq!(row.setting(), None, "{}", row.title());
                assert!(!row.chosen(), "{}", row.title());
                assert!(row.entries().is_none(), "{}", row.title());
            }
            assert_eq!(
                addressing[2].typed().map(|typed| typed.value.as_str()),
                Some("10.0.0.2/24")
            );
            assert_eq!(
                addressing[2].typed().map(|typed| typed.about),
                Some(Typing::Network {
                    device: intern(SOCKET),
                    field: crate::network::Field::Address
                })
            );
            assert_eq!(addressing[2].comment(), Some("10.0.0.2/24"));
            assert_eq!(
                addressing[3].typed().map(|typed| typed.about),
                Some(Typing::Network {
                    device: intern(SOCKET),
                    field: crate::network::Field::Router
                })
            );
            // The row above says what it is set to, so the usual question is
            // answered without stepping in.
            assert_eq!(
                wired
                    .iter()
                    .find(|entry| entry.title() == "IP address")
                    .and_then(Entry::comment),
                Some("Manual — 10.0.0.2/24")
            );
        });
    }

    /// Manual needs an address to pin, and a machine that has none is told so
    /// rather than offered a press that `NetworkManager` refuses.
    #[test]
    fn manual_is_explained_where_there_is_nothing_to_pin() {
        let fresh = crate::network::Device {
            address: None,
            ipv4: crate::network::Ipv4 {
                automatic: true,
                can_pin: false,
                ..Default::default()
            },
            ..a_socket()
        };
        with_network(reported(vec![fresh], Vec::new()), || {
            let page = network_page();
            let addressing = under(under(&page, "Wired"), "IP address").to_vec();
            assert_eq!(titles(&addressing), ["Automatic", "Manual"]);
            assert!(addressing[0].chosen());
            assert_eq!(
                addressing[1].setting(),
                None,
                "a press NetworkManager would refuse is not offered"
            );
            assert!(addressing[1]
                .comment()
                .is_some_and(|note| note.contains("no address to pin")));
        });
    }

    /// A device with no saved profile has nothing to configure, and the page
    /// says which of the two nothings that is.
    #[test]
    fn addressing_belongs_to_a_profile_rather_than_to_a_socket() {
        let unsaved = crate::network::Device {
            profile: None,
            ipv4: crate::network::Ipv4::default(),
            ..a_socket()
        };
        with_network(reported(vec![unsaved], Vec::new()), || {
            let page = network_page();
            let wired = under(&page, "Wired");
            let addressing = under(wired, "IP address").to_vec();
            assert_eq!(titles(&addressing), ["No connection to configure"]);
            assert_eq!(addressing[0].setting(), None);
            // And no name servers page at all: there is nothing to hang them on
            // either, and a second row saying the same thing is a second row
            // saying the same thing.
            assert_eq!(
                titles(wired),
                ["Connection", "IP address", "Connection information"]
            );
        });
    }

    /// The DNS page, and the one place its shape follows the addressing: a
    /// pinned profile runs no DHCP, so there is nothing for its name servers to
    /// be automatic *from*.
    #[test]
    fn dns_is_only_automatic_where_there_is_something_to_ask() {
        with_network(reported(vec![a_socket()], Vec::new()), || {
            let page = network_page();
            let servers = under(under(&page, "Wired"), "DNS").to_vec();
            assert_eq!(titles(&servers), ["Automatic", "Manual"]);
            assert!(servers[0].chosen());
            assert_eq!(
                servers[1].setting(),
                Some(Setting::Network(NetworkValue::Dns {
                    device: intern(SOCKET),
                    automatic: false
                }))
            );
        });

        // Chosen manually under automatic addressing: the list appears under
        // the two answers rather than as a row of the page above them.
        let named = crate::network::Device {
            ipv4: crate::network::Ipv4 {
                automatic: true,
                dns_automatic: false,
                dns: vec!["9.9.9.9".to_string(), "1.1.1.1".to_string()],
                can_pin: true,
                ..Default::default()
            },
            ..a_socket()
        };
        with_network(reported(vec![named], Vec::new()), || {
            let page = network_page();
            let wired = under(&page, "Wired");
            let servers = under(wired, "DNS").to_vec();
            assert_eq!(
                titles(&servers),
                ["Automatic", "Manual", "DNS servers"],
                "the field is named in full, because the panel it opens is"
            );
            assert!(servers[1].chosen());
            assert_eq!(
                servers[2].typed().map(|typed| typed.value.as_str()),
                Some("9.9.9.9, 1.1.1.1"),
                "every one of them in the field, in the order they are asked"
            );
            assert_eq!(
                wired
                    .iter()
                    .find(|entry| entry.title() == "DNS")
                    .and_then(Entry::comment),
                Some("9.9.9.9, 1.1.1.1")
            );
        });

        // And under a pinned address there is no choice to offer, so the page
        // says why instead of offering an Automatic that would mean none.
        let pinned = crate::network::Device {
            ipv4: crate::network::Ipv4 {
                automatic: false,
                address: Some("10.0.0.2/24".to_string()),
                dns_automatic: true,
                can_pin: true,
                ..Default::default()
            },
            ..a_socket()
        };
        with_network(reported(vec![pinned], Vec::new()), || {
            let page = network_page();
            let servers = under(under(&page, "Wired"), "DNS").to_vec();
            assert_eq!(titles(&servers), ["Always manual here", "DNS servers"]);
            assert_eq!(servers[0].setting(), None);
            assert_eq!(
                servers[1].typed().map(|typed| typed.value.as_str()),
                Some(""),
                "an empty field rather than no field: none set is a thing to see"
            );
        });
    }

    /// Both halves get the same two pages, which is the whole of what was asked
    /// for: a static address is not a thing that is true of a cable and false of
    /// a radio.
    ///
    /// They hang in different places, and that is not an inconsistency. A
    /// socket has one profile and the addressing is a property of the socket as
    /// far as anybody using it is concerned. A radio has one profile *per
    /// network* — which is why a laptop keeps a fixed address at the office and
    /// takes whatever it is given at home — so the only honest place for it is
    /// under the network it belongs to.
    #[test]
    fn a_radio_is_addressed_exactly_as_a_socket_is() {
        use crate::network::Security;
        let networks = vec![a_network("Upstairs", 62, Security::Personal, true, true)];
        with_network(
            reported(
                vec![a_socket(), a_radio()],
                vec![(RADIO.to_string(), networks)],
            ),
            || {
                let page = network_page();
                let wired = under(&page, "Wired");
                assert_eq!(
                    titles(wired),
                    ["Connection", "IP address", "DNS", "Connection information"]
                );
                let joined = under(under(under(&page, "Wi-Fi"), "Networks"), "Upstairs");
                assert_eq!(
                    titles(joined),
                    ["IP address", "DNS", "Disconnect", "Forget"],
                    "the addressing stands above the two rows that act, so a \
                     column that opens on its first row opens on one that only \
                     opens another"
                );

                // And each names its own device, so setting one does not touch
                // the other.
                assert_eq!(
                    under(wired, "IP address")[1].setting(),
                    Some(Setting::Network(NetworkValue::Addressing {
                        device: intern(SOCKET),
                        automatic: false
                    }))
                );
                assert_eq!(
                    under(joined, "IP address")[1].setting(),
                    Some(Setting::Network(NetworkValue::Addressing {
                        device: intern(RADIO),
                        automatic: false
                    }))
                );
            },
        );
    }

    /// Every field says whose value it is, because the panel it opens covers
    /// the trail that would otherwise have said so.
    ///
    /// `Address` on its own is the same panel whether the user walked in
    /// through the socket on the back of the machine or through the network the
    /// radio is on, and those are two different profiles with two different
    /// addresses. See [`crate::apps::Typed::whose`].
    #[test]
    fn a_typed_value_says_which_connection_it_belongs_to() {
        use crate::network::Security;
        let pinned = crate::network::Ipv4 {
            automatic: false,
            address: Some("10.0.0.2/24".to_string()),
            gateway: Some("10.0.0.1".to_string()),
            dns_automatic: false,
            dns: vec!["9.9.9.9".to_string()],
            can_pin: true,
        };
        let socket = crate::network::Device {
            ipv4: pinned.clone(),
            ..a_socket()
        };
        let radio = crate::network::Device {
            ipv4: pinned,
            ..a_radio()
        };
        with_network(
            reported(
                vec![socket, radio],
                vec![(
                    RADIO.to_string(),
                    vec![a_network("Upstairs", 62, Security::Personal, true, true)],
                )],
            ),
            || {
                let page = network_page();
                let whose = |rows: &[Entry], title: &str| {
                    under(rows, title)
                        .iter()
                        .filter_map(Entry::typed)
                        .map(|typed| typed.whose.clone())
                        .collect::<Vec<_>>()
                };
                // The socket's is the profile NetworkManager files it under,
                // which is the name every other tool on the machine shows.
                let wired = under(&page, "Wired");
                assert_eq!(
                    whose(wired, "IP address"),
                    ["A wired profile", "A wired profile"]
                );
                assert_eq!(whose(wired, "DNS"), ["A wired profile"]);

                // The radio's is the network's own name, which is the thing the
                // user chose and the thing the profile is about.
                let joined = under(under(under(&page, "Wi-Fi"), "Networks"), "Upstairs");
                assert_eq!(whose(joined, "IP address"), ["Upstairs", "Upstairs"]);
                assert_eq!(whose(joined, "DNS"), ["Upstairs"]);
            },
        );
    }

    /// A socket with no profile to file it under is named by its interface,
    /// which is the only other name the thing has. A field titled `Address`
    /// with nothing after it would be a field about nothing.
    #[test]
    fn a_field_with_no_profile_name_falls_back_to_the_interface() {
        let unnamed = crate::network::Device {
            connection: None,
            ipv4: crate::network::Ipv4 {
                automatic: false,
                address: Some("10.0.0.2/24".to_string()),
                can_pin: true,
                ..Default::default()
            },
            ..a_socket()
        };
        with_network(reported(vec![unnamed], Vec::new()), || {
            let page = network_page();
            assert_eq!(
                under(under(&page, "Wired"), "IP address")
                    .iter()
                    .filter_map(Entry::typed)
                    .map(|typed| typed.whose.as_str())
                    .next(),
                Some("test-wired0")
            );
        });
    }

    /// A radio that is on nothing has no addressing page anywhere, and that is
    /// the honest answer rather than an oversight: there is no profile for the
    /// values to belong to, and a page offering them would be setting an
    /// address on whichever network the card happened to have saved.
    #[test]
    fn a_radio_on_nothing_has_no_addressing_to_offer() {
        use crate::network::Security;
        let idle = crate::network::Device {
            link: crate::network::Link::Idle,
            connection: None,
            ..a_radio()
        };
        with_network(
            reported(
                vec![idle],
                vec![(
                    RADIO.to_string(),
                    vec![a_network("Upstairs", 62, Security::Personal, true, false)],
                )],
            ),
            || {
                let page = network_page();
                let wifi = under(&page, "Wi-Fi");
                assert_eq!(
                    titles(wifi),
                    ["Wi-Fi", "Networks", "Connection information"]
                );
                let networks = under(wifi, "Networks");
                assert_eq!(titles(networks), ["Upstairs"]);
                // Saved, so it opens — but on the two things there are to do
                // with a network the radio is not on, and neither of them is
                // an address. The addressing belongs to the profile the radio
                // is connected *by*, and it is connected by none.
                let saved = under(networks, "Upstairs");
                assert_eq!(titles(saved), ["Connect", "Forget"]);
                assert!(
                    !networks[0].chosen(),
                    "a network the machine merely knows is not one it is on"
                );
                assert_eq!(
                    saved[0].setting(),
                    Some(Setting::Network(NetworkValue::Join {
                        device: intern(RADIO),
                        ssid: intern("Upstairs"),
                    }))
                );
            },
        );
    }

    /// Highlighting a network must not join it. It would take the machine off
    /// what it is on and put it on whatever the cursor was passing —
    /// mid-download, mid-call — and on a secured one it would raise a password
    /// panel for a row nobody chose.
    #[test]
    fn walking_over_a_network_changes_nothing() {
        theme::with_accent("Purple", || {
            preview(Some(Setting::Network(NetworkValue::Radio(false))));
            preview(Some(Setting::Network(NetworkValue::Join {
                device: intern(RADIO),
                ssid: intern("Upstairs"),
            })));
            preview(Some(Setting::Network(NetworkValue::Wire {
                device: intern(SOCKET),
                up: false,
            })));
            theme::animate(1.0);
            assert_eq!(theme::accent().name, "Purple");
        });
    }

    /// A network row is `NetworkManager`'s to carry out and to remember, so
    /// nothing about it reaches the settings file — exactly as nothing about a
    /// sound device does, and for the same reason.
    #[test]
    fn a_network_is_never_written_down() {
        with_network(reported(vec![a_radio()], Vec::new()), || {
            let before = stored();
            let mut written = false;
            assert!(apply_with(
                Setting::Network(NetworkValue::Join {
                    device: intern(RADIO),
                    ssid: intern("Upstairs"),
                }),
                |_| written = true,
            ));
            assert!(!written, "a network reached the settings file");
            assert_eq!(stored(), before);
        });
    }

    /// The bands, as a person names them — and nothing at all for a number in
    /// none of them, because a row reading "5745 MHz" is the shell reading a
    /// register out loud.
    #[test]
    fn which_band_a_network_was_heard_on() {
        assert_eq!(band_of(2412), Some("2.4 GHz"));
        assert_eq!(band_of(2484), Some("2.4 GHz"));
        assert_eq!(band_of(5180), Some("5 GHz"));
        assert_eq!(band_of(5825), Some("5 GHz"));
        assert_eq!(band_of(6115), Some("6 GHz"));
        assert_eq!(band_of(0), None);
        assert_eq!(band_of(900), None);
    }

    // --- Bluetooth ---------------------------------------------------------

    /// The handles BlueZ would use, written here rather than taken from
    /// anything on this machine: a controller path or an address pasted out of
    /// a live listing is a test that says the page was built for one desk.
    const HCI: &str = "/org/bluez/hci-test0";
    const HCI_ADDRESS: &str = "00:00:5E:00:53:00";
    const PADS: &str = "/org/bluez/hci-test0/dev_00_00_5E_00_53_01";
    const EARS: &str = "/org/bluez/hci-test0/dev_00_00_5E_00_53_02";
    const STRANGER: &str = "/org/bluez/hci-test0/dev_00_00_5E_00_53_03";

    fn a_controller() -> crate::bluetooth::Controller {
        crate::bluetooth::Controller {
            path: HCI.to_string(),
            interface: "hci-test0".to_string(),
            name: "A machine".to_string(),
            address: HCI_ADDRESS.to_string(),
            powered: true,
            switchable: true,
            discovering: false,
            discoverable: false,
        }
    }

    fn a_device(
        path: &str,
        name: &str,
        kind: crate::bluetooth::Kind,
        paired: bool,
        connected: bool,
    ) -> crate::bluetooth::Device {
        crate::bluetooth::Device {
            path: path.to_string(),
            controller: HCI.to_string(),
            address: "00:00:5E:00:53:01".to_string(),
            name: name.to_string(),
            kind,
            paired,
            connected,
            strength: Some(-55),
            battery: None,
            doing: None,
        }
    }

    /// A listing with whichever halves the test is about, and nothing else.
    fn heard(
        controllers: Vec<crate::bluetooth::Controller>,
        devices: Vec<(String, Vec<crate::bluetooth::Device>)>,
    ) -> crate::bluetooth::Listing {
        crate::bluetooth::Listing {
            manager: true,
            controllers,
            devices,
            wanted: None,
        }
    }

    /// One controller with these devices on it, which is nearly every machine.
    fn one_controller(devices: Vec<crate::bluetooth::Device>) -> crate::bluetooth::Listing {
        heard(vec![a_controller()], vec![(HCI.to_string(), devices)])
    }

    /// The Bluetooth column, wherever it has got to in the tree.
    fn bluetooth_page() -> Vec<Entry> {
        let column = column();
        let row = column
            .iter()
            .find(|entry| entry.title() == "Bluetooth")
            .expect("Settings has a Bluetooth row");
        row.entries().expect("Bluetooth opens a column").to_vec()
    }

    /// The whole of what the user asked for when there is nothing to draw: a
    /// machine with no controller says Bluetooth is not available, in words,
    /// with the mark every read-only explanation in this tree wears.
    ///
    /// Both reasons, because they are not the same fact: a machine with no
    /// controller in it will never have Bluetooth until somebody plugs one in,
    /// and a session whose Bluetooth service is not running is a machine that
    /// has it and has nothing in charge of it.
    #[test]
    fn a_machine_with_no_controller_says_bluetooth_is_not_available() {
        for manager in [false, true] {
            let mut listing = heard(Vec::new(), Vec::new());
            listing.manager = manager;
            with_bluetooth(listing, || {
                let page = bluetooth_page();
                assert_eq!(titles(&page), ["Bluetooth is not available"]);
                // Nothing to choose and no mark to move: this row describes the
                // machine, and a row describing something true must not be one
                // the user can un-choose.
                assert_eq!(page[0].setting(), None);
                assert!(!page[0].chosen());
                assert!(page[0].entries().is_none());
                assert_eq!(page[0].icon(), Some(icons::SETTING_INFO));
                assert!(
                    page[0].comment().is_some_and(|why| why.len() > 20),
                    "and it says why"
                );
            });
        }

        // The two reasons read differently, which is the whole point of asking
        // separately.
        let with_service = heard(Vec::new(), Vec::new());
        let mut without = with_service.clone();
        without.manager = false;
        let mut reasons = Vec::new();
        for listing in [with_service, without] {
            with_bluetooth(listing, || {
                reasons.push(bluetooth_page()[0].comment().unwrap().to_string());
            });
        }
        assert_ne!(reasons[0], reasons[1]);
    }

    /// Three rows and no controller in sight: the page is a switch, what the
    /// machine is talking to, and what the machine itself is.
    ///
    /// The `hci0`, `hci1` column this replaced is the thing being asserted
    /// against. A machine with two radios in it must not put the kernel's name
    /// for either of them in front of somebody who came to turn Bluetooth on.
    #[test]
    fn the_page_is_a_switch_a_list_and_the_machine_itself() {
        let second = crate::bluetooth::Controller {
            path: "/org/bluez/hci-test1".to_string(),
            interface: "hci-test1".to_string(),
            address: "00:00:5E:00:53:FF".to_string(),
            ..a_controller()
        };
        with_bluetooth(heard(vec![a_controller(), second], Vec::new()), || {
            assert_eq!(
                titles(&bluetooth_page()),
                ["Bluetooth", "Devices", "Configuration"]
            );
        });
    }

    /// The devices are gone while the controller is off — there is nothing to
    /// list — but the page is not, because what this machine *is* over
    /// Bluetooth has an answer whether the radio is on or not.
    #[test]
    fn the_devices_are_gone_while_the_controller_is_off() {
        let mut controller = a_controller();
        controller.powered = false;
        with_bluetooth(heard(vec![controller], Vec::new()), || {
            let page = bluetooth_page();
            assert_eq!(titles(&page), ["Bluetooth", "Configuration"]);
            let switch = under(&page, "Bluetooth");
            assert_eq!(titles(switch), ["Off", "On"], "Off is above On, as ever");
            assert!(switch[0].chosen(), "it is off and the mark says so");
            assert_eq!(
                switch[1].setting(),
                Some(Setting::Bluetooth(BluetoothValue::Power {
                    controller: intern(HCI),
                    on: true
                }))
            );
        });
    }

    /// A controller the machine has switched off in hardware is not offered a
    /// switch at all — the argument is the wireless radio's, unchanged: On would
    /// be accepted, nothing would happen, and the mark would come to rest on a
    /// row describing a machine that is not this one.
    #[test]
    fn a_controller_killed_in_hardware_is_explained_rather_than_offered() {
        let mut controller = a_controller();
        controller.switchable = false;
        with_bluetooth(heard(vec![controller], Vec::new()), || {
            let page = bluetooth_page();
            assert_eq!(
                titles(&page),
                ["Bluetooth is off at the machine", "Configuration"]
            );
            assert_eq!(page[0].setting(), None);
        });
    }

    /// The Devices column is what the machine knows, with the one way to add to
    /// it standing above them.
    ///
    /// A stranger in the air is **not** in this column. That is the difference
    /// from the wireless page it is otherwise a copy of, and the reason is that
    /// the two lists answer different questions: a network in the air is a thing
    /// to join, and a device in the air is a thing to decide about. A list where
    /// the headphones somebody uses every day sit among the beacons from the
    /// flat upstairs is a list they have to search every time.
    #[test]
    fn the_devices_column_is_what_the_machine_knows() {
        let devices = vec![
            a_device(EARS, "Ears", crate::bluetooth::Kind::Headphones, true, true),
            a_device(PADS, "Pad", crate::bluetooth::Kind::Controller, true, false),
            a_device(
                STRANGER,
                "Something",
                crate::bluetooth::Kind::Unknown,
                false,
                false,
            ),
        ];
        with_bluetooth(one_controller(devices), || {
            let page = under(&bluetooth_page(), "Devices").to_vec();
            assert_eq!(titles(&page), ["Search to pair", "Ears", "Pad"]);
            // The stranger is one step further in, where the scan is.
            let searching = under(&page, "Search to pair");
            assert_eq!(titles(searching), ["Something"]);
            assert!(matches!(
                searching[0].setting(),
                Some(Setting::Bluetooth(BluetoothValue::Connect { .. }))
            ));
            assert_eq!(
                searching[0].icon(),
                Some(icons::SWATCH),
                "one press, no column"
            );
        });
    }

    /// The search column is never empty, and it cannot be: the bar will not
    /// step into an empty column, and a scan that only starts on the way in
    /// would then be one that could never start at all.
    #[test]
    fn the_search_column_always_has_something_to_step_into() {
        with_bluetooth(
            one_controller(vec![a_device(
                EARS,
                "Ears",
                crate::bluetooth::Kind::Headphones,
                true,
                false,
            )]),
            || {
                let page = under(&bluetooth_page(), "Devices").to_vec();
                let searching = under(&page, "Search to pair");
                assert_eq!(searching.len(), 1);
                assert_eq!(searching[0].setting(), None, "and it does nothing");
                assert!(searching[0].comment().is_some_and(|note| !note.is_empty()));
                // The row above it invites rather than reports: what is behind
                // it is not a count of anything until somebody presses it.
                assert_eq!(
                    page[0].comment(),
                    Some("Look for something new to pair with")
                );
                assert_eq!(page[0].icon(), Some(icons::SEARCH));
            },
        );
    }

    /// The two shapes a device the machine knows has, and what pressing each of
    /// them does.
    #[test]
    fn a_known_device_is_pressed_according_to_what_it_already_is() {
        let devices = vec![
            a_device(EARS, "Ears", crate::bluetooth::Kind::Headphones, true, true),
            a_device(PADS, "Pad", crate::bluetooth::Kind::Controller, true, false),
        ];
        with_bluetooth(one_controller(devices), || {
            let page = under(&bluetooth_page(), "Devices").to_vec();

            // Connected: the tick, and the way off it behind the row.
            let ears_row = &page[1];
            assert!(ears_row.chosen(), "the one it is on carries the mark");
            assert_eq!(
                ears_row.setting(),
                None,
                "pressing it opens rather than acts"
            );
            let ears = under(&page, "Ears");
            assert_eq!(
                titles(ears),
                ["Device information", "Disconnect", "Forget"],
                "the row a second Accept lands on has to be one that does nothing"
            );
            assert_eq!(
                ears[1].setting(),
                Some(Setting::Bluetooth(BluetoothValue::Disconnect {
                    device: intern(EARS)
                }))
            );
            assert!(ears[1].acts() && ears[2].acts(), "neither takes a mark");

            // Paired and off: Connect first, so the column opens on it.
            assert!(!page[2].chosen());
            let pad = under(&page, "Pad");
            assert_eq!(titles(pad), ["Connect", "Device information", "Forget"]);
            assert_eq!(
                pad[0].setting(),
                Some(Setting::Bluetooth(BluetoothValue::Connect {
                    device: intern(PADS)
                }))
            );
            assert_eq!(
                pad[2].setting(),
                Some(Setting::Bluetooth(BluetoothValue::Forget {
                    device: intern(PADS)
                }))
            );
        });
    }

    /// A paired device the controller cannot hear is still listed, and says so.
    ///
    /// The other half of the argument the search page makes: a network out of
    /// range cannot be listed because it is not in the air, but headphones in a
    /// drawer are a thing the machine is still paired with — and forgetting them
    /// is exactly what somebody wants when the device is not to hand.
    #[test]
    fn a_paired_device_out_of_range_is_listed_and_says_so() {
        let mut ears = a_device(
            EARS,
            "Ears",
            crate::bluetooth::Kind::Headphones,
            true,
            false,
        );
        ears.strength = None;
        with_bluetooth(one_controller(vec![ears]), || {
            let page = under(&bluetooth_page(), "Devices").to_vec();
            assert_eq!(titles(&page), ["Search to pair", "Ears"]);
            assert_eq!(page[1].comment(), Some("Paired — not in range"));
            assert!(
                under(&page, "Ears")
                    .iter()
                    .any(|row| row.title() == "Forget"),
                "the row somebody came here for is there"
            );
        });
    }

    /// A press that is still being carried out is what the row says, above
    /// everything else about it: it is the only thing on the page the user is
    /// actually waiting on.
    #[test]
    fn a_device_being_connected_to_says_so() {
        let mut ears = a_device(
            EARS,
            "Ears",
            crate::bluetooth::Kind::Headphones,
            true,
            false,
        );
        ears.doing = Some(crate::bluetooth::Doing::Connecting);
        with_bluetooth(one_controller(vec![ears]), || {
            let page = under(&bluetooth_page(), "Devices").to_vec();
            assert_eq!(page[1].comment(), Some("Connecting…"));
        });
    }

    /// What a device is, as a page to read — and the reading behind the row's
    /// word for it, which is where a number in dBm belongs and the only place
    /// it does.
    #[test]
    fn what_a_device_is_is_a_panel_to_read() {
        let mut ears = a_device(EARS, "Ears", crate::bluetooth::Kind::Headphones, true, true);
        ears.battery = Some(80);
        with_bluetooth(one_controller(vec![ears]), || {
            let page = under(&bluetooth_page(), "Devices").to_vec();
            assert_eq!(page[1].comment(), Some("Connected — 80% battery"));
            let row = under(&page, "Ears")[0].clone();
            assert_eq!(row.setting(), None);
            assert!(row.entries().is_none());
            let Some(crate::apps::About::Listed(values)) =
                row.facts().map(|facts| facts.about.clone())
            else {
                panic!("the values travel with the row rather than being read on the press");
            };
            assert_eq!(
                values
                    .iter()
                    .map(|(label, value)| (label.as_str(), value.as_str()))
                    .collect::<Vec<_>>(),
                [
                    ("Kind", "Headphones"),
                    ("Address", "00:00:5E:00:53:01"),
                    ("Battery", "80%"),
                    ("Signal", "-55 dBm"),
                ]
            );
        });
    }

    /// What the machine itself is: five rows on a machine with two radios, four
    /// on the ordinary one.
    ///
    /// The controller row is the one that comes and goes, and it collapses under
    /// the same rule the wireless and wired pages collapse under: a list of one
    /// is not a choice. What it would have said is on the row below it anyway.
    #[test]
    fn configuration_is_what_this_machine_is() {
        with_bluetooth(one_controller(Vec::new()), || {
            let page = under(&bluetooth_page(), "Configuration").to_vec();
            assert_eq!(
                titles(&page),
                ["Name", "Address", "Visibility", "On startup"]
            );
            // The name is typed rather than chosen, and it opens with what it
            // already is: nobody should have to type a machine's name out again
            // to change one letter of it.
            let name = page[0].typed().expect("the name is a typed row");
            assert_eq!(name.value, "A machine");
            assert_eq!(name.comment, "A machine");
            assert_eq!(
                name.about,
                Typing::BluetoothName {
                    controller: intern(HCI)
                }
            );
            // The address is a fact, so a row that shows it rather than a door
            // in front of one line.
            assert_eq!(page[1].comment(), Some(HCI_ADDRESS));
            assert_eq!(page[1].setting(), None);
            assert_eq!(page[1].icon(), Some(icons::SETTING_INFO));
        });
    }

    /// A machine with two radios in it chooses between them here, by address —
    /// which is the only thing that tells them apart, because BlueZ calls every
    /// adapter in a machine after the machine.
    #[test]
    fn two_controllers_are_chosen_between_by_address() {
        let second = crate::bluetooth::Controller {
            path: "/org/bluez/hci-test1".to_string(),
            interface: "hci-test1".to_string(),
            address: "00:00:5E:00:53:FF".to_string(),
            powered: false,
            ..a_controller()
        };
        with_bluetooth(heard(vec![a_controller(), second], Vec::new()), || {
            let page = under(&bluetooth_page(), "Configuration").to_vec();
            assert_eq!(
                titles(&page),
                ["Controller", "Name", "Address", "Visibility", "On startup"]
            );
            let controllers = under(&page, "Controller");
            assert_eq!(titles(controllers), ["Controller 1", "Controller 2"]);
            assert!(
                controllers[0].chosen(),
                "nothing has been chosen, so the first one is the one in use"
            );
            // The address is on the row, because a number on its own would be
            // asking somebody to pick between two things they cannot tell apart.
            assert!(controllers[1]
                .comment()
                .is_some_and(|note| note.starts_with("00:00:5E:00:53:FF")));
            assert_eq!(
                controllers[1].setting(),
                Some(Setting::Bluetooth(BluetoothValue::Use {
                    address: intern("00:00:5E:00:53:FF")
                }))
            );
        });
    }

    /// Choosing a controller moves everything above it: the switch, the name and
    /// the devices are all about the radio in force.
    ///
    /// And it is remembered by **address**, so the kernel renumbering the
    /// adapters cannot quietly move the preference to the other radio.
    #[test]
    fn the_chosen_controller_is_the_one_the_whole_page_is_about() {
        let second = crate::bluetooth::Controller {
            path: "/org/bluez/hci-test1".to_string(),
            interface: "hci-test1".to_string(),
            address: "00:00:5E:00:53:FF".to_string(),
            name: "The other one".to_string(),
            ..a_controller()
        };
        let listing = heard(vec![a_controller(), second], Vec::new());
        with_bluetooth(listing, || {
            let before = stored();
            assert!(apply_with(
                Setting::Bluetooth(BluetoothValue::Use {
                    address: intern("00:00:5E:00:53:FF")
                }),
                |_| {},
            ));
            let page = bluetooth_page();
            // The switch now names the other radio's path.
            assert_eq!(
                under(&page, "Bluetooth")[1].setting(),
                Some(Setting::Bluetooth(BluetoothValue::Power {
                    controller: intern("/org/bluez/hci-test1"),
                    on: true
                }))
            );
            let configuration = under(&page, "Configuration").to_vec();
            assert_eq!(configuration[2].comment(), Some("00:00:5E:00:53:FF"));

            // Unlike everything else about Bluetooth, this one is written down:
            // BlueZ has no opinion about which of two radios the user means.
            assert_ne!(stored(), before);
            assert_eq!(
                stored().bluetooth_controller.as_deref(),
                Some("00:00:5E:00:53:FF")
            );
            // And a radio that has been unplugged does not leave the page
            // blank — the one still in the machine is still Bluetooth.
            let alone = heard(vec![a_controller()], Vec::new());
            assert_eq!(
                chosen_controller(&alone).map(|controller| controller.address.as_str()),
                Some(HCI_ADDRESS)
            );
        });
    }

    /// Visibility is off by default and says why there is nothing to press when
    /// the radio is off: a machine nothing can hear cannot be found either.
    #[test]
    fn visibility_is_a_switch_unless_the_radio_is_off() {
        with_bluetooth(one_controller(Vec::new()), || {
            let page = under(&bluetooth_page(), "Configuration").to_vec();
            let visibility = under(&page, "Visibility");
            assert_eq!(titles(visibility), ["Off", "On"]);
            assert!(visibility[0].chosen(), "off until somebody says otherwise");
            assert_eq!(
                visibility[1].setting(),
                Some(Setting::Bluetooth(BluetoothValue::Visible {
                    controller: intern(HCI),
                    on: true
                }))
            );
        });

        let mut controller = a_controller();
        controller.powered = false;
        controller.discoverable = false;
        with_bluetooth(heard(vec![controller], Vec::new()), || {
            let page = under(&bluetooth_page(), "Configuration").to_vec();
            let row = page
                .iter()
                .find(|entry| entry.title() == "Visibility")
                .expect("the row is still there");
            assert_eq!(row.setting(), None, "and there is nothing to press");
            assert!(row.entries().is_none());
        });
    }

    /// What happens at startup is three answers, not two, and the third is the
    /// default: a machine whose owner has never been to this page is left alone.
    #[test]
    fn the_startup_policy_is_remembered() {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());
        let saved = take_settings();
        note_bluetooth(one_controller(Vec::new()));

        let page = || {
            let column = column();
            let row = column
                .iter()
                .find(|entry| entry.title() == "Bluetooth")
                .expect("Settings has a Bluetooth row");
            row.entries().expect("it opens a column").to_vec()
        };
        let startup = |rows: &[Entry]| {
            rows.iter()
                .find(|entry| entry.title() == "Configuration")
                .and_then(Entry::entries)
                .expect("Configuration opens a column")
                .iter()
                .find(|entry| entry.title() == "On startup")
                .and_then(Entry::entries)
                .expect("On startup opens a column")
                .to_vec()
        };

        let rows = page();
        let choices = startup(&rows);
        assert_eq!(titles(&choices), ["Off", "On", "As it was left"]);
        assert!(
            choices[2].chosen(),
            "the one that decides nothing is the default"
        );

        // Chosen, written down, and read back by the next session.
        let mut written = None;
        assert!(apply_with(
            Setting::Bluetooth(BluetoothValue::Startup(Startup::On)),
            |stored| written = stored.bluetooth_startup.clone(),
        ));
        assert_eq!(written.as_deref(), Some("on"));
        assert_eq!(bluetooth_startup(), Startup::On);
        let rows = page();
        assert!(startup(&rows)[1].chosen());

        // And what "as it was left" refers to is written by the shell watching
        // rather than by anybody pressing anything. Through the half that
        // decides, never the half that writes: the whole file belongs to
        // whoever is running these tests.
        assert!(remember_bluetooth_powered(false), "false is news here");
        assert!(!bluetooth_was_on());
        assert_eq!(stored().bluetooth_was_on, Some(false));
        assert!(
            !remember_bluetooth_powered(false),
            "and saying it twice is not, which is what spares the file"
        );

        put_back(saved);
    }

    /// Nothing about a *pairing* reaches the settings file. BlueZ is what
    /// remembers one, for the reason the sound server remembers a device and
    /// `NetworkManager` a network.
    #[test]
    fn a_pairing_is_not_written_down() {
        let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());
        let before = stored();
        for value in [
            BluetoothValue::Connect {
                device: intern(EARS),
            },
            BluetoothValue::Disconnect {
                device: intern(EARS),
            },
            BluetoothValue::Forget {
                device: intern(EARS),
            },
            BluetoothValue::Power {
                controller: intern(HCI),
                on: true,
            },
            BluetoothValue::Visible {
                controller: intern(HCI),
                on: true,
            },
        ] {
            let mut written = false;
            assert!(apply_with(Setting::Bluetooth(value), |_| written = true));
            assert!(!written, "{value:?} reached the settings file");
        }
        assert_eq!(stored(), before);
    }

    /// How far off something is, in the words a person would use — and nothing
    /// at all for a device that has not been heard.
    #[test]
    fn how_far_off_a_device_is() {
        assert_eq!(nearness(Some(-40)), Some("close by"));
        assert_eq!(nearness(Some(-60)), Some("close by"));
        assert_eq!(nearness(Some(-61)), Some("nearby"));
        assert_eq!(nearness(Some(-75)), Some("nearby"));
        assert_eq!(nearness(Some(-76)), Some("far away"));
        assert_eq!(nearness(None), None);
    }
}
