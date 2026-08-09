//! The Settings column: the shell's own rows, rather than anything installed.
//!
//! Everything else in the bar is a consequence of what is on disk — a
//! `.desktop` file sorted into a column. This one is written here, because
//! what it holds are LineXinBar's own controls, and a shell that had to find its
//! own settings on the filesystem could be left without them.
//!
//! It is a tree, and deliberately: the real XMB kept its Settings region a
//! short column of subcategories rather than one long list, so a setting is
//! always two or three rows away instead of thirty. Each level here is a
//! [`Folder`], which is what the bar steps into.
//!
//! A row that sets something carries a [`Setting`] saying what. The bar knows
//! how to move a mark from one row of a column to another — that is
//! [`Cursor::choose`] — and nothing more; what the value *means* is applied
//! here, in [`apply`], which is also where it is written down so the next
//! session comes up the way this one was left.
//!
//! Two kinds of setting live here, and they are not applied the same way. The
//! accent is the shell's own and takes effect in the next frame it draws.
//! Everything under Display belongs to the *compositor* — a colour pipeline on
//! a CRTC and an infoframe on a connector, neither of which a client may touch
//! — so what this module does with it is record it, and `main` sends it over
//! `lxb_shell_v1` for the compositor to carry out.
//!
//! Those settings are also *per display*, all the way down: one screen can be
//! an HDR television and the next a laptop panel, and the two want different
//! answers. So the tree names the screen before it offers a setting, and every
//! [`Setting`] under Display carries which screen it belongs to.
//!
//! [`Cursor::choose`]: crate::model::Cursor::choose

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use crate::apps::{Category, Choice, Entry, Folder};
use crate::icons;
use crate::theme::{self, Color};

/// What choosing a row does.
///
/// The row that carries it is generic — a column of values looks like any
/// other column — so the shell has to be able to tell one list of values from
/// the next without reading their titles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// Set the accent to the palette of this name — one of [`theme::ACCENTS`].
    Accent(&'static str),
    /// Change one display's picture. Carries the connector the change belongs
    /// to, because every one of these is a property of one screen.
    Display {
        display: &'static str,
        value: DisplayValue,
    },
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
    /// Drive this display in high dynamic range, or stop.
    Hdr(bool),
    /// The luminance plain white is sent at while HDR is on, in cd/m².
    SdrBrightness(u16),
    /// How far sRGB's colours are stretched towards BT.2020's, 0 to 100.
    SrgbIntensity(u8),
    /// Peak luminance declared to the display, in cd/m². 0 asks for whatever
    /// the display says about itself.
    PeakBrightness(u16),
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

/// Write down that a shelf is listed in this order from now on.
pub fn remember_media_sort(kind: crate::media::Kind, sort: crate::media::Sort) {
    MEDIA_SORT.lock().unwrap().insert(
        crate::apps::shelf_title(kind).to_string(),
        sort.key().to_string(),
    );
    save(&stored());
}

/// What the settings file says a shelf is listed in, if it says anything.
///
/// Asked once, when the library is made. An order the shell does not have —
/// a hand-edited typo, or a file written by a later version — is nothing
/// rather than an error: the shelf comes up alphabetical, which is the answer
/// that is never wrong.
pub fn media_sort(kind: crate::media::Kind) -> Option<crate::media::Sort> {
    let held = MEDIA_SORT.lock().unwrap();
    let named = held.get(crate::apps::shelf_title(kind))?;
    let sort = crate::media::Sort::from_key(named);
    if sort.is_none() {
        tracing::warn!(
            shelf = crate::apps::shelf_title(kind),
            order = named,
            "the settings name an order this shell does not have"
        );
    }
    sort
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

/// Connector names, kept alive for as long as the process is.
///
/// A [`Setting`] has to name the display it belongs to and stay `Copy`: every
/// row of the bar carries one by value, and the catalogue holding those rows is
/// cloned, walked and compared all over the shell, so an owned `String` in
/// there would ripple out through the model, the layout and the input path. A
/// connector's name is fixed for as long as it exists, there are single digits
/// of them, and they are already alive for the whole session — so they are
/// interned once each and never freed.
fn intern(name: &str) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut names = NAMES.lock().unwrap();
    if let Some(known) = names.iter().find(|known| **known == name) {
        return known;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    names.push(leaked);
    leaked
}

/// The rows of the Settings column, in the order they appear under it.
pub fn column() -> Vec<Entry> {
    vec![appearance(), display()]
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
    let (id, ..) = crate::apps::SHELL_SETTINGS;
    if let Some(settings) = categories.iter_mut().find(|category| category.id == id) {
        settings.entries = column();
    }
}

/// How the shell looks: for now, the one colour everything chosen is drawn in.
fn appearance() -> Entry {
    folder(
        "Appearance",
        "How the shell looks",
        icons::SETTING_APPEARANCE,
        vec![accent_colour()],
    )
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
/// with them. Everything under HDR describes the picture those carry.
fn display() -> Entry {
    folder(
        "Display",
        "How the picture reaches the screen",
        icons::SETTING_DISPLAY,
        vec![
            resolution(),
            refresh_rate(),
            orientation(),
            high_dynamic_range(),
        ],
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

fn setting(display: &'static str, value: DisplayValue) -> Setting {
    Setting::Display { display, value }
}

fn folder(title: &str, comment: &str, icon: &str, entries: Vec<Entry>) -> Entry {
    Entry::Folder(Folder {
        title: title.to_string(),
        comment: Some(comment.to_string()),
        icon: Some(icon.to_string()),
        entries,
    })
}

/// One colour in a list of them: named, drawn in itself, and marked when it is
/// the one in force.
fn swatch(title: &str, colour: Color, chosen: bool, setting: Setting) -> Entry {
    Entry::Choice(Choice {
        title: title.to_string(),
        comment: None,
        icon: Some(icons::SWATCH.to_string()),
        swatch: Some(colour),
        chosen,
        setting: Some(setting),
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
        chosen,
        setting: Some(setting),
    })
}

/// Something the shell can show but not change.
///
/// It carries no [`Setting`], so choosing it does nothing and no mark moves.
/// That is the whole point of the distinction: a row that describes something
/// true must not be one the user can un-choose.
fn reading(title: &str, note: &str) -> Entry {
    Entry::Choice(Choice {
        title: title.to_string(),
        comment: Some(note.to_string()),
        icon: Some(icons::SETTING_INFO.to_string()),
        swatch: None,
        chosen: false,
        setting: None,
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
pub fn preview(setting: Option<Setting>) {
    match setting {
        Some(Setting::Accent(name)) => {
            if !theme::preview_accent(name) {
                tracing::warn!(accent = name, "no accent by that name");
            }
        }
        // Highlighting a value the compositor would have to act on changes
        // nothing; the accent goes back to what is applied, as it does when
        // the cursor leaves a list of values entirely.
        Some(Setting::Display { .. }) | None => theme::restore_accent(),
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
/// tested without one.
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

fn apply_with(setting: Setting, persist: impl FnOnce(&Stored)) -> bool {
    match setting {
        Setting::Accent(name) => {
            if !theme::commit_accent(name) {
                tracing::warn!(accent = name, "no accent by that name");
                return false;
            }
            tracing::info!(accent = name, "accent");
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
                        | DisplayValue::Orientation(_) => {}
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
    adopt(stored);
}

/// Take the display settings out of a parsed file.
///
/// Split from [`load`] so the file format can be exercised without one.
fn adopt(stored: Stored) {
    *MEDIA_SORT.lock().unwrap() = stored.media_sort;

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
    held.clear();
    modes.clear();
    turns.clear();
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
    hdr: Option<bool>,
    hdr_sdr_brightness: Option<u16>,
    hdr_srgb_intensity: Option<u8>,
    hdr_peak_brightness: Option<u16>,
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

    Stored {
        accent: Some(theme::accent().name.to_string()),
        hdr: Some(inherited.enabled),
        hdr_sdr_brightness: Some(inherited.sdr_brightness),
        hdr_srgb_intensity: Some(inherited.srgb_intensity),
        hdr_peak_brightness: Some(inherited.peak_brightness),
        display,
        media_sort: MEDIA_SORT.lock().unwrap().clone(),
    }
}

impl StoredDisplay {
    fn hdr_from(&mut self, hdr: Hdr) {
        self.hdr = Some(hdr.enabled);
        self.hdr_sdr_brightness = Some(hdr.sdr_brightness);
        self.hdr_srgb_intensity = Some(hdr.srgb_intensity);
        self.hdr_peak_brightness = Some(hdr.peak_brightness);
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
    }
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
# hdr:                  drive this display in high dynamic range.
# hdr-sdr-brightness:   what plain white is sent at, in cd/m².
# hdr-srgb-intensity:   how far sRGB colour is stretched towards BT.2020,
#                       0 for the colour SDR showed, 100 for vivid. Needs a
#                       driver with a degamma stage; where there is none the
#                       compositor says so and this has no effect.
# hdr-peak-brightness:  the peak declared to the display, in cd/m².
#                       0 means whatever the display says about itself.
#
# [media-sort] is what order the rows of the user's own files are listed in,
# one key per shelf — Music, Video, Images — chosen from the Sort row of the
# context menu over any file in them. The orders are: name, name-reversed,
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
        };
        HDR.lock().unwrap().clear();
        MODE.lock().unwrap().clear();
        TURN.lock().unwrap().clear();
        note_turned(Vec::new());
        *INHERITED.lock().unwrap() = Hdr::default();
        saved
    }

    fn put_back(saved: Saved) {
        *HDR.lock().unwrap() = saved.hdr;
        *INHERITED.lock().unwrap() = saved.inherited;
        *MODE.lock().unwrap() = saved.mode;
        *TURN.lock().unwrap() = saved.turn;
        note_support(saved.support);
        note_modes(saved.offered);
        note_turned(saved.reported_turns);
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
                            // `controls_for` walks, and neither half of a mode
                            // nor the turn is one of them.
                            DisplayValue::Resolution(_)
                            | DisplayValue::RefreshRate(_)
                            | DisplayValue::Orientation(_) => {
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
                    hdr: Some(true),
                    hdr_sdr_brightness: Some(250),
                    hdr_srgb_intensity: Some(50),
                    hdr_peak_brightness: Some(1000),
                },
            )]),
            media_sort: BTreeMap::from([
                ("Images".to_string(), "created-newest-first".to_string()),
                ("Music".to_string(), "type".to_string()),
            ]),
            ..Stored::default()
        };
        let body = format!("{PREAMBLE}{}", toml::to_string_pretty(&written).unwrap());
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
        });
        assert!(body.contains("[media-sort]"), "{body}");

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
                ..Stored::default()
            });
            assert_eq!(media_sort(crate::media::Kind::Audio), None);
        });

        // A file cut down to nothing still parses, and says nothing.
        assert_eq!(toml::from_str::<Stored>("").unwrap(), Stored::default());
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
                    ["Resolution", "Refresh rate", "Orientation", "HDR"],
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
}
