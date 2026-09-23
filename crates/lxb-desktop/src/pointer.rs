//! Driving the pointer from the right stick, and remembering which
//! applications it is turned on for.
//!
//! A console has a controller and no mouse, and plenty of what a user wants to
//! run on one was written for a mouse: a launcher's own settings page, a
//! browser, an emulator's menu. The right stick is the one control a
//! fullscreen application is least likely to need at the moment the user has
//! stopped playing and started pointing at something — but it is not free
//! either, because it is a game's camera, which is exactly why this is a
//! choice made per application rather than a mode the shell is in.
//!
//! Two halves, and they are separate on purpose:
//!
//! * [`Stick`] turns a stick position into pointer movement. It knows about
//!   dead zones and how a stick feels, and nothing about Wayland.
//! * [`Prefs`] remembers the answer per application, on disk, so that turning
//!   it on for a browser once does not have to be done again tomorrow.
//!
//! The movement itself goes to the compositor over `lxb_shell_v1`: only
//! the compositor may speak for the seat, and the point is to reach the
//! application, not to draw a cursor of the shell's own on top of it. Where
//! the pointer is then allowed to go is the compositor's half of the same
//! answer — inside that application's window and nowhere else, since this
//! stick was turned on for one application and a second display is not
//! somewhere it was ever aimed.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How far the stick has to be pushed before it is being pushed at all.
///
/// Larger than the navigation dead zone in [`crate::controller`], and
/// deliberately: that one is asking a yes/no question about a direction, where
/// a false positive costs one wrong menu row, and this one is integrating
/// continuously, where a stick resting a hair off centre would walk the cursor
/// off the screen over a minute of nobody touching it.
const DEAD_ZONE: f32 = 0.18;

/// How fast the pointer travels at full deflection, in logical pixels per
/// second.
///
/// Set against the width of a screen rather than a number that felt right in
/// isolation: about a second and a half to cross a 1080p display, which is
/// close to what a mouse takes at a comfortable hand speed and slow enough
/// that the far edge is not overshot on the way to a menu.
const POINTER_SPEED: f32 = 1250.0;

/// How fast a floating window travels at full deflection, in logical pixels per
/// second.
///
/// Slower than the pointer, and set against what is being moved rather than
/// against the screen. A pointer is a mark a few pixels across being aimed at
/// something small; a window is a hand's worth of screen being put somewhere,
/// and there are only ever a few places on a display it is going. About two and
/// a half seconds to cross a 1080p panel, which is unhurried enough that a
/// corner can be arrived at rather than overshot.
const WINDOW_SPEED: f32 = 750.0;

/// The same for scrolling, in logical pixels of content per second.
///
/// Slower than the pointer, and set against a different thing: roughly a
/// screenful every second and a half at full deflection, which is a page of
/// text read at a skim rather than a list thrown past the eye. A wheel notch
/// is fifteen pixels in most toolkits, so this is about four notches a second
/// — near enough what a finger does on a wheel it is turning steadily.
const SCROLL_SPEED: f32 = 900.0;

/// The response curve's exponent. Anything above 1 spends more of the stick's
/// travel on slow movement, which is where every fiddly pointing job lives —
/// the last few pixels onto a checkbox — and leaves the speed at the rim
/// unchanged.
const CURVE: f32 = 2.2;

/// Longest single step to integrate, in seconds.
///
/// A shell that has been stalled — a slow frame, a display being plugged in —
/// must not answer with one enormous jump the moment it comes back, so a gap
/// longer than this counts as this much. The pointer arrives late rather than
/// somewhere else entirely.
const MAX_STEP: f32 = 0.05;

/// Whether there is a thumb on a stick at all, on exactly the terms
/// [`Stick::motion`] answers `None` for.
///
/// A different question from whether the stick *moved* anything, and both are
/// wanted: a stick held perfectly still at full deflection produces travel every
/// poll, and one held still at rest produces none — but so does the first poll
/// after a gap, which has no interval behind it. Anything that takes hold of
/// something while a stick is pushed has to ask this rather than reading the
/// travel, or it lets go once a poll and takes hold again on the next.
pub fn pushed((x, y): (f32, f32)) -> bool {
    (x * x + y * y).sqrt() > DEAD_ZONE
}

/// Turns stick deflection into movement, at whatever rate it was built for.
///
/// One type for the two of them because they are the same instrument: a
/// deflection, a dead zone, a curve and an interval. What differs is only how
/// far a full push goes in a second, and whether the number that comes out is
/// the pointer's travel or the content's.
#[derive(Debug)]
pub struct Stick {
    /// When motion was last integrated, as seconds since the shell started.
    /// `None` until the first sample, which therefore moves nothing: the first
    /// frame has no interval behind it.
    last: Option<f32>,
    /// Logical pixels a second at full deflection.
    speed: f32,
}

impl Stick {
    /// The right stick, aiming the pointer.
    pub fn pointer() -> Self {
        Self {
            last: None,
            speed: POINTER_SPEED,
        }
    }

    /// The right stick again, carrying a floating window over the guide.
    ///
    /// The same instrument at a different rate, which is the whole of why there
    /// is one type here: a window follows the stick with the same dead zone and
    /// the same curve the pointer does, so the control feels like one control
    /// whichever of the two it happens to be moving.
    pub fn floating_window() -> Self {
        Self {
            last: None,
            speed: WINDOW_SPEED,
        }
    }

    /// The left stick and the D-pad, scrolling what is under it.
    pub fn scroll() -> Self {
        Self {
            last: None,
            speed: SCROLL_SPEED,
        }
    }

    /// The movement `at` seconds, given where the stick is now.
    ///
    /// `x` and `y` are the stick's own axes, positive right and — as every
    /// gamepad API normalises them — positive *up*, which is why the vertical
    /// result is negated: a screen's y grows downwards.
    ///
    /// Returns `None` while the stick is inside its dead zone, which is the
    /// common case by a wide margin: it is what stops an untouched controller
    /// sending a stream of zero-sized movements to the compositor.
    pub fn motion(&mut self, x: f32, y: f32, at: f32) -> Option<(f64, f64)> {
        let elapsed = self.last.replace(at).map(|last| at - last);
        // A radial dead zone, not a pair of axis ones: a square cut-out lets a
        // stick pushed diagonally register while the same push straight up
        // does not, and the diagonal is where a stick spends its rest.
        let magnitude = (x * x + y * y).sqrt();
        if magnitude <= DEAD_ZONE {
            return None;
        }
        // Rescaled so the movement starts from nothing at the dead zone's edge
        // rather than jumping to whatever the curve says the raw magnitude is.
        // Without this the pointer sets off at a tenth of full speed the
        // instant the stick is touched, which reads as a dead zone that is
        // both too large and not there at all.
        let scaled = ((magnitude - DEAD_ZONE) / (1.0 - DEAD_ZONE)).clamp(0.0, 1.0);
        let speed = scaled.powf(CURVE) * self.speed;

        // Direction from the raw axes, speed from the curve: the two are kept
        // apart so that a curve steep enough to be useful cannot also bend the
        // direction the stick is being pushed in.
        let step = elapsed.unwrap_or(0.0).clamp(0.0, MAX_STEP);
        let distance = speed * step / magnitude;
        let (dx, dy) = (x * distance, -y * distance);
        (dx != 0.0 || dy != 0.0).then_some((dx as f64, dy as f64))
    }

    /// Forget when the last sample was, so the next one starts a fresh
    /// interval instead of integrating the gap.
    ///
    /// Called whenever the pointer stops being driven — the menu opening, the
    /// application exiting, the toggle going off — because the time the stick
    /// spent not being read is not time the pointer should catch up on.
    pub fn rest(&mut self) {
        self.last = None;
    }
}

/// What the shell remembers about one application.
///
/// A table rather than a bare boolean because the tile beside the first of
/// these is a per-application volume, which belongs in exactly the same place
/// under exactly the same key — and because the second arrived and did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct AppPrefs {
    /// Whether the right stick moves the pointer inside this application.
    ///
    /// Left out of the file while it is off. Until there was a second setting
    /// an entry existed only because this was on, so `false` was never written;
    /// now an application can have a line for its resolution alone, and a
    /// `stick-pointer = false` under it would be a choice nobody made.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub stick_pointer: bool,
    /// How many pixels this application draws its picture at, where somebody
    /// has chosen. `None` is the display's own size, which is what everything
    /// runs at until it is told otherwise — see [`crate::resolution`].
    ///
    /// Two numbers rather than a named size, because what is remembered has to
    /// go on meaning something on a screen this shell has never seen: the menu
    /// offers what fits the display it was raised on, and a machine plugged
    /// into a different television answers the same question again from the
    /// same numbers.
    pub resolution: Option<[u32; 2]>,
}

impl AppPrefs {
    /// Whether this is worth writing down. An application the user has only
    /// ever looked at must not earn a line in the file.
    fn is_set(&self) -> bool {
        *self != Self::default()
    }
}

/// Per-application settings, as they are held and as they are stored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Keyed by the application's own identity — the `app_id` a toplevel sets,
    /// or an X11 window's class, as the compositor reports it. Never the
    /// window title, which is a document name and changes under the setting.
    ///
    /// A Steam game is one of these and needs nothing special: Valve starts a
    /// title under a class of its own making, `steam_app_<id>`, which is an
    /// application identity like any other and is the same one every time that
    /// game runs.
    apps: BTreeMap<String, AppPrefs>,
    /// The same, for one of the user's own games out of their ROM folder.
    ///
    /// Keyed by the file the game *is*, because there is nothing else to key it
    /// by: every game in that folder is played by the same emulator, under the
    /// same window name, so a setting filed the way an application's is would
    /// be a setting on the emulator and would follow whichever game was
    /// started next. The path is what distinguishes one game from another, and
    /// it is what the row on the bar stands for.
    ///
    /// Which means the shell, rather than the compositor, is what makes the
    /// setting land on the right game: the emulator is told what to draw at on
    /// the way into each launch. See `Shell::push_resolution_for_a_launch`.
    games: BTreeMap<String, AppPrefs>,
    /// Where this was read from, and where it will be written back. Not part
    /// of the file.
    #[serde(skip)]
    path: Option<PathBuf>,
}

impl Prefs {
    /// Read them, or start empty.
    ///
    /// A missing file is the normal first run. An unreadable or malformed one
    /// is reported and then also treated as empty: the settings this holds are
    /// conveniences, and refusing to start a session over them would be a much
    /// worse failure than forgetting which applications had the stick pointer
    /// on.
    pub fn load() -> Self {
        let Some(path) = prefs_path() else {
            tracing::debug!("no config directory; per-application settings are not persisted");
            return Self::default();
        };
        let mut prefs = match std::fs::read_to_string(&path) {
            Ok(raw) => match toml::from_str::<Self>(&raw) {
                Ok(prefs) => {
                    tracing::info!(
                        path = %path.display(),
                        applications = prefs.apps.len(),
                        "read per-application settings"
                    );
                    prefs
                }
                Err(err) => {
                    tracing::warn!(%err, path = %path.display(), "ignoring unreadable settings");
                    Self::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                tracing::warn!(%err, path = %path.display(), "could not read settings");
                Self::default()
            }
        };
        prefs.path = Some(path);
        prefs
    }

    /// Whether the right stick moves the pointer in `app`.
    ///
    /// An empty key is an application that told nobody what it is, and nothing
    /// can be remembered about it: it would otherwise share one entry with
    /// every other nameless window on the machine.
    pub fn stick_pointer(&self, app: &str) -> bool {
        if app.is_empty() {
            return false;
        }
        self.apps.get(app).is_some_and(|prefs| prefs.stick_pointer)
    }

    /// Turn it on or off for `app`, and write the file.
    ///
    /// Returns the new setting, which is what the caller draws. `false` when
    /// there is no application to attach it to — nothing changes then, and
    /// nothing is written.
    pub fn set_stick_pointer(&mut self, app: &str, on: bool) -> bool {
        if app.is_empty() {
            return false;
        }
        let entry = self.apps.entry(app.to_string()).or_default();
        if entry.stick_pointer == on {
            return on;
        }
        entry.stick_pointer = on;
        // An application back at its defaults leaves no trace, so the file
        // stays a list of what the user has actually chosen rather than of
        // everything they have ever run.
        self.apps.retain(|_, prefs| prefs.is_set());
        tracing::info!(app, on, "stick pointer");
        self.save();
        on
    }

    /// Toggle it, and report where it ended up.
    pub fn toggle_stick_pointer(&mut self, app: &str) -> bool {
        self.set_stick_pointer(app, !self.stick_pointer(app))
    }

    /// How many pixels `app` draws its picture at, or `None` for the display's
    /// own size.
    ///
    /// An empty key is answered `None` on the terms [`Self::stick_pointer`]
    /// answers `false`: an application that told nobody what it is has nothing
    /// for a setting to be filed under.
    pub fn resolution(&self, app: &str) -> Option<[u32; 2]> {
        self.apps.get(app.trim()).and_then(|prefs| prefs.resolution)
    }

    /// Choose one for `app`, and write the file. `None` puts it back to the
    /// display's own size, which leaves no entry behind.
    ///
    /// Answers whether anything changed, which is what the caller tells the
    /// compositor and redraws on. A key of nothing changes nothing and writes
    /// nothing.
    pub fn set_resolution(&mut self, app: &str, resolution: Option<[u32; 2]>) -> bool {
        let app = app.trim();
        if app.is_empty() {
            return false;
        }
        if self.resolution(app) == resolution {
            return false;
        }
        self.apps.entry(app.to_string()).or_default().resolution = resolution;
        self.apps.retain(|_, prefs| prefs.is_set());
        tracing::info!(app, ?resolution, "the resolution this application draws at");
        self.save();
        true
    }

    /// The same, for one of the user's own games, keyed by the file it is.
    ///
    /// Its own pair of functions rather than a flag on the two above, because
    /// what differs is not how the setting behaves but what it is filed under —
    /// and a single map holding both would be a map whose keys mean two things,
    /// one of which happens to be a path. See [`Self::games`].
    pub fn game_resolution(&self, game: &std::path::Path) -> Option<[u32; 2]> {
        self.games
            .get(game.to_str()?)
            .and_then(|prefs| prefs.resolution)
    }

    /// Choose one for a game, and write the file. `true` when it is a change.
    ///
    /// A path this machine cannot write down as text is refused rather than
    /// lossily converted: a key that is not exactly the file is a key that
    /// would answer for a different game tomorrow.
    pub fn set_game_resolution(
        &mut self,
        game: &std::path::Path,
        resolution: Option<[u32; 2]>,
    ) -> bool {
        let Some(key) = game.to_str() else {
            tracing::warn!(path = %game.display(), "this game's name cannot be written down");
            return false;
        };
        if self.game_resolution(game) == resolution {
            return false;
        }
        self.games.entry(key.to_string()).or_default().resolution = resolution;
        self.games.retain(|_, prefs| prefs.is_set());
        tracing::info!(game = key, ?resolution, "the resolution this game plays at");
        self.save();
        true
    }

    /// Write the file, through a temporary and a rename.
    ///
    /// Atomically, because this is written from the guide overlay on a machine
    /// whose power button is three rows below the switch that triggers it: a
    /// half-written file is one the next session cannot read at all, and it
    /// would take every other application's settings with it.
    fn save(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let Some(directory) = path.parent() else {
            return;
        };
        if let Err(err) = std::fs::create_dir_all(directory) {
            tracing::warn!(%err, path = %directory.display(), "could not create the config directory");
            return;
        }

        let body = match toml::to_string_pretty(self) {
            Ok(body) => format!("{PREAMBLE}{body}"),
            Err(err) => {
                tracing::warn!(%err, "could not serialise the per-application settings");
                return;
            }
        };

        let temporary = path.with_extension("toml.new");
        if let Err(err) = std::fs::write(&temporary, body) {
            tracing::warn!(%err, path = %temporary.display(), "could not write the settings");
            return;
        }
        if let Err(err) = std::fs::rename(&temporary, path) {
            tracing::warn!(%err, path = %path.display(), "could not replace the settings");
            let _ = std::fs::remove_file(&temporary);
        }
    }
}

/// What the file says about itself, since it is written by the shell but sits
/// next to a file the user is expected to edit by hand.
const PREAMBLE: &str = "\
# Per-application settings, written by the LineXinBar shell.
#
# Applications are keyed by the name they give themselves: an xdg_toplevel's
# app_id, or an X11 window's class. Editing this by hand is fine; the shell
# reads it once at startup and rewrites it whenever a setting changes.
#
# One application may appear under more than one of its names. A setting made
# before it has ever run is filed under the name its desktop entry says its
# windows will use, and the compositor is told about every name that entry
# could go by, so the two cannot miss each other.
#
# The games below are the user's own, out of the ROM folder, and are keyed by
# the file rather than by a name: every one of them is played by the same
# emulator under the same window name.

";

/// `$XDG_CONFIG_HOME/lxb/apps.toml`, beside the compositor's own config.
fn prefs_path() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from)?;
            Some(home.join(".config"))
        })?;
    Some(config.join("lxb").join("apps.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The interval matters as much as the deflection: the same stick position
    /// for twice as long is twice the movement, which is what makes the
    /// pointer travel at a speed rather than at a frame rate.
    #[test]
    fn movement_is_a_speed_rather_than_a_step_per_poll() {
        let mut stick = Stick::pointer();
        // The first sample has no interval behind it, so it moves nothing.
        assert_eq!(stick.motion(1.0, 0.0, 0.0), None);

        let (short, _) = stick.motion(1.0, 0.0, 0.01).unwrap();
        let (long, _) = stick.motion(1.0, 0.0, 0.03).unwrap();
        assert!(
            (long - short * 2.0).abs() < 1e-6,
            "twice the interval is twice the distance: {short} then {long}"
        );
    }

    /// The rest position of a real stick is never exactly centre, and a
    /// pointer that drifts across the screen while nobody is touching the
    /// controller is worse than one that is switched off.
    #[test]
    fn a_stick_at_rest_moves_nothing() {
        let mut stick = Stick::pointer();
        stick.motion(0.0, 0.0, 0.0);
        for drift in [0.0, 0.05, 0.12, DEAD_ZONE] {
            assert_eq!(stick.motion(drift, drift * 0.5, 1.0), None, "{drift}");
        }
        // And a diagonal rest is inside the dead zone too, which an axis-wise
        // cut-out would have let through.
        assert_eq!(stick.motion(0.12, 0.12, 2.0), None);
    }

    /// Movement has to start from nothing at the dead zone's edge. A curve
    /// applied to the raw magnitude sets off at a tenth of full speed the
    /// instant the stick leaves rest, which reads as no dead zone at all.
    #[test]
    fn speed_comes_up_from_zero_at_the_edge_of_the_dead_zone() {
        let mut stick = Stick::pointer();
        stick.motion(0.0, 0.0, 0.0);
        let (just_out, _) = stick.motion(DEAD_ZONE + 0.01, 0.0, 0.02).unwrap();
        assert!(just_out > 0.0 && just_out < 0.5, "{just_out}");

        stick.rest();
        stick.motion(0.0, 0.0, 1.0);
        let (full, _) = stick.motion(1.0, 0.0, 1.02).unwrap();
        assert!(
            (full - (POINTER_SPEED * 0.02) as f64).abs() < 1e-3,
            "{full}"
        );
    }

    /// Screens count y downwards and gamepads count it upwards, and the one
    /// place that has to be reconciled is here.
    #[test]
    fn pushing_the_stick_up_moves_the_pointer_up_the_screen() {
        let mut stick = Stick::pointer();
        stick.motion(0.0, 0.0, 0.0);
        let (dx, dy) = stick.motion(0.0, 1.0, 0.02).unwrap();
        assert_eq!(dx, 0.0);
        assert!(dy < 0.0, "up the screen is a negative y: {dy}");

        stick.rest();
        stick.motion(0.0, 0.0, 1.0);
        let (dx, dy) = stick.motion(-1.0, 0.0, 1.02).unwrap();
        assert!(dx < 0.0, "{dx}");
        assert_eq!(dy, -0.0);
    }

    /// A stalled shell must not answer with one enormous jump when it comes
    /// back: the pointer should be late, not somewhere else.
    #[test]
    fn a_long_gap_is_capped_rather_than_integrated() {
        let mut stick = Stick::pointer();
        stick.motion(1.0, 0.0, 0.0);
        let (jump, _) = stick.motion(1.0, 0.0, 4.0).unwrap();
        assert!(
            (jump - (POINTER_SPEED * MAX_STEP) as f64).abs() < 1e-3,
            "{jump}"
        );
    }

    /// Scrolling is the same instrument at a different rate, and the rate is
    /// the only thing that differs: the dead zone, the curve and the interval
    /// are one implementation, so a fix to any of them reaches both.
    #[test]
    fn scrolling_is_the_same_stick_at_its_own_speed() {
        let (mut pointing, mut scrolling) = (Stick::pointer(), Stick::scroll());
        pointing.motion(0.0, 0.0, 0.0);
        scrolling.motion(0.0, 0.0, 0.0);

        let (pointer, _) = pointing.motion(1.0, 0.0, 0.02).unwrap();
        let (scroll, _) = scrolling.motion(1.0, 0.0, 0.02).unwrap();
        assert!(
            (scroll / pointer - (SCROLL_SPEED / POINTER_SPEED) as f64).abs() < 1e-3,
            "{scroll} against {pointer}"
        );

        // And the dead zone is the same dead zone, so a resting stick scrolls
        // no more than it points.
        assert_eq!(scrolling.motion(0.1, 0.1, 0.04), None);
    }

    /// Coming back after the menu, or after the application exited, must not
    /// integrate the whole time the stick was not being read.
    #[test]
    fn resting_starts_a_fresh_interval() {
        let mut stick = Stick::pointer();
        stick.motion(1.0, 0.0, 0.0);
        stick.rest();
        assert_eq!(
            stick.motion(1.0, 0.0, 9.0),
            None,
            "the first sample after a rest has no interval behind it"
        );
    }

    /// Diagonals travel at the same speed as the cardinals, which is what
    /// keeps a circular sweep of the stick a circle on screen.
    #[test]
    fn the_speed_does_not_depend_on_the_direction() {
        let mut stick = Stick::pointer();
        stick.motion(0.0, 0.0, 0.0);
        let (right, _) = stick.motion(1.0, 0.0, 0.02).unwrap();
        stick.rest();
        stick.motion(0.0, 0.0, 1.0);
        let diagonal = std::f32::consts::FRAC_1_SQRT_2;
        let (dx, dy) = stick.motion(diagonal, diagonal, 1.02).unwrap();
        let travelled = (dx * dx + dy * dy).sqrt();
        assert!((travelled - right).abs() < 1e-3, "{travelled} vs {right}");
    }

    #[test]
    fn a_setting_is_remembered_per_application() {
        let mut prefs = Prefs::default();
        assert!(!prefs.stick_pointer("org.kde.kwrite"));

        assert!(prefs.toggle_stick_pointer("org.kde.kwrite"));
        assert!(prefs.stick_pointer("org.kde.kwrite"));
        // One application's answer is not every application's.
        assert!(!prefs.stick_pointer("celeste"));

        assert!(!prefs.toggle_stick_pointer("org.kde.kwrite"));
        assert!(!prefs.stick_pointer("org.kde.kwrite"));
    }

    /// An application that never said what it is cannot be remembered:
    /// otherwise every nameless window on the machine shares one entry, and
    /// turning the pointer on in one of them turns it on in all of them.
    #[test]
    fn a_nameless_application_is_not_remembered() {
        let mut prefs = Prefs::default();
        assert!(!prefs.toggle_stick_pointer(""));
        assert!(!prefs.stick_pointer(""));
        assert!(prefs.apps.is_empty());
    }

    /// The file is a list of what the user chose, not of everything they have
    /// run: an application switched back to its defaults leaves no line.
    #[test]
    fn turning_a_setting_back_off_drops_the_entry() {
        let mut prefs = Prefs::default();
        prefs.toggle_stick_pointer("firefox");
        assert_eq!(prefs.apps.len(), 1);
        prefs.toggle_stick_pointer("firefox");
        assert!(prefs.apps.is_empty());
    }

    /// What is written has to be what is read back, including through the
    /// kebab-case the file is authored in.
    #[test]
    fn settings_survive_a_round_trip_through_the_file() {
        let mut prefs = Prefs::default();
        prefs.toggle_stick_pointer("org.mozilla.firefox");

        let body = toml::to_string_pretty(&prefs).unwrap();
        assert!(body.contains("stick-pointer = true"), "{body}");

        let read: Prefs = toml::from_str(&body).unwrap();
        assert!(read.stick_pointer("org.mozilla.firefox"));
        assert!(!read.stick_pointer("something-else"));
    }

    /// A file from a newer shell, or one somebody has typed a stray key into,
    /// must not cost the user the settings that are still readable — and must
    /// never stop the session coming up.
    #[test]
    fn unknown_fields_and_missing_ones_are_both_survivable() {
        let read: Prefs = toml::from_str(
            r#"
            [apps."org.kde.kwrite"]
            stick-pointer = true

            [apps."org.kde.dolphin"]
            "#,
        )
        .unwrap();
        assert!(read.stick_pointer("org.kde.kwrite"));
        assert!(!read.stick_pointer("org.kde.dolphin"));

        // And an empty file is simply nobody having chosen anything yet.
        assert_eq!(toml::from_str::<Prefs>("").unwrap(), Prefs::default());
    }

    /// A resolution is two numbers under the same key everything else about an
    /// application is under, and it survives the round trip through the file.
    #[test]
    fn a_resolution_is_written_down_and_read_back() {
        let mut prefs = Prefs::default();
        assert_eq!(prefs.resolution("steam_app_42"), None);
        assert!(prefs.set_resolution("steam_app_42", Some([1280, 720])));
        assert_eq!(prefs.resolution("steam_app_42"), Some([1280, 720]));

        // Saying the same thing again is not a change, which is what stops the
        // compositor being asked to lay every window out for nothing.
        assert!(!prefs.set_resolution("steam_app_42", Some([1280, 720])));

        let written = toml::to_string_pretty(&prefs).unwrap();
        assert_eq!(toml::from_str::<Prefs>(&written).unwrap(), prefs);
        assert!(
            !written.contains("stick-pointer"),
            "a switch nobody turned on was written down:\n{written}"
        );
    }

    /// Back at the display's own size leaves no trace. The file is a list of
    /// what somebody has chosen, not of everything they have ever run.
    #[test]
    fn going_back_to_native_leaves_no_entry_behind() {
        let mut prefs = Prefs::default();
        prefs.set_resolution("puzzle", Some([960, 540]));
        assert!(prefs.set_resolution("puzzle", None));
        assert_eq!(prefs.resolution("puzzle"), None);
        assert_eq!(prefs, Prefs::default(), "an entry was left behind");

        // And an application with something *else* chosen keeps its line.
        let mut both = Prefs::default();
        both.set_stick_pointer("browser", true);
        both.set_resolution("browser", Some([1280, 720]));
        both.set_resolution("browser", None);
        assert!(both.stick_pointer("browser"));
    }

    /// One of the user's own games is filed under the file it is, and is a
    /// different key from anything an application could be filed under: every
    /// game in that folder is played by the same emulator under the same
    /// window name, so filing them together would be filing them all as one.
    #[test]
    fn a_game_is_remembered_by_its_file_and_not_by_the_emulator() {
        let mut prefs = Prefs::default();
        let one = std::path::Path::new("/games/snes/one.sfc");
        let two = std::path::Path::new("/games/snes/two.sfc");
        assert!(prefs.set_game_resolution(one, Some([960, 720])));
        assert_eq!(prefs.game_resolution(one), Some([960, 720]));
        assert_eq!(prefs.game_resolution(two), None);
        // And it is nothing to do with the emulator's own entry.
        assert_eq!(prefs.resolution("retroarch"), None);

        let written = toml::to_string_pretty(&prefs).unwrap();
        assert_eq!(toml::from_str::<Prefs>(&written).unwrap(), prefs);
    }

    /// A key of nothing is refused, on the terms the stick pointer refuses
    /// one: an application that told nobody what it is has nothing for a
    /// setting to be filed under, and one entry shared by every nameless
    /// window would be worse than none.
    #[test]
    fn nothing_cannot_be_given_a_resolution() {
        let mut prefs = Prefs::default();
        assert!(!prefs.set_resolution("", Some([1280, 720])));
        assert!(!prefs.set_resolution("   ", Some([1280, 720])));
        assert_eq!(prefs, Prefs::default());
    }
}
