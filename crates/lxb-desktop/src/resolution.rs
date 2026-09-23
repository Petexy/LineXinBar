//! How many pixels an application draws its picture at.
//!
//! A console has one answer to "what does this run at" and it is the
//! television's, which is the right answer for almost everything and the wrong
//! one for the handful of titles a machine cannot quite keep up with. So the
//! shell offers the other answer per application: draw at 1280×720 and let the
//! compositor put that picture over the whole screen, which cuts the work of a
//! frame to a third and costs sharpness rather than smoothness.
//!
//! It is carried out entirely by the compositor —
//! `lxb_shell_v1.set_application_resolution`, and `crate::scale::Resolution`
//! on the other side of it — and that is what makes it work for every kind of
//! thing on this bar. An installed application, a game out of somebody's Steam
//! library and a game out of their ROM folder are started by three different
//! programs and only one of them is this shell; what they have in common is a
//! window on this compositor, which is where the setting is applied.
//!
//! This module is the shell's half: which sizes are worth offering on a given
//! screen, and what one of them is called. Where a choice is *filed* is
//! [`crate::pointer::Prefs`], beside the other thing the shell remembers per
//! application.
//!
//! # Which sizes are offered
//!
//! Only sizes that share the shape of the screen they will be drawn on. The
//! compositor fits a picture of the wrong shape inside the display rather than
//! stretching it — the proportions the application drew in are the one thing
//! worth protecting when something has gone wrong — but a menu that offered
//! 1024×768 on a widescreen television would be offering black bars as a
//! setting, and nobody wants those. So the shape is the filter, and it is
//! exact: a size is offered when its width times the display's height equals
//! its height times the display's width, which is integer arithmetic and has no
//! tolerance to get wrong.
//!
//! Two sources pass through that filter. The first is [`LADDER`], the sizes
//! people already have names for, so that a 1080p screen offers 720p rather
//! than some proportion of itself. The second is the plain fractions of the
//! display's own size, for the screens no ladder anticipates: an ultrawide, a
//! portrait panel, a nested window somebody is testing in. Between them every
//! screen gets something, and neither can produce a size that is not exactly
//! the screen's shape.

/// The sizes people already have a name for, largest first.
///
/// Every one of them is exactly the shape of one of the aspect ratios screens
/// are sold in — 16∶9, 16∶10, 4∶3, 3∶2 and the two ultrawides — because the
/// filter below is exact and a row that is a pixel off the display's shape is a
/// row nobody would ever be offered. `1366×768` is the famous example of one
/// that is not: it is 683∶384, not 16∶9, and it is left out rather than quietly
/// pillarboxed.
///
/// Sorted here rather than at the point of use, so that the list somebody reads
/// on the menu is in the order this is written in.
pub const LADDER: &[[u32; 2]] = &[
    // 16∶9
    [3200, 1800],
    [2560, 1440],
    [1920, 1080],
    [1600, 900],
    [1280, 720],
    [1024, 576],
    [960, 540],
    [640, 360],
    // 16∶10
    [2560, 1600],
    [1920, 1200],
    [1680, 1050],
    [1440, 900],
    [1280, 800],
    [960, 600],
    [640, 400],
    // 4∶3
    [1600, 1200],
    [1400, 1050],
    [1280, 960],
    [1024, 768],
    [800, 600],
    [640, 480],
    // 3∶2
    [1920, 1280],
    [1440, 960],
    [1200, 800],
    [960, 640],
    [720, 480],
    // The two ultrawides, 64∶27 and 43∶18
    [2560, 1080],
    [1920, 810],
    [3440, 1440],
    [1720, 720],
];

/// The fractions of the display's own size that are tried when the ladder has
/// nothing to say about a screen.
///
/// Three quarters, two thirds and a half — far enough apart that each is
/// visibly a different choice, and each a fraction somebody can hold in their
/// head. Only where the arithmetic comes out whole in both directions, since a
/// rounded fraction is no longer exactly the screen's shape and the point of
/// these is that they always are.
const FRACTIONS: &[(u32, u32)] = &[(3, 4), (2, 3), (1, 2)];

/// The sizes worth offering for an application on a display this size, largest
/// first.
///
/// The display's own size is not among them. It is the answer for an
/// application nobody has chosen for and it is drawn as its own row — see the
/// menu that calls this — rather than as the top of a list of alternatives:
/// what that row means is "whatever this screen is", which goes on meaning the
/// right thing when the screen is changed underneath it, and a remembered
/// 1920×1080 would not.
///
/// Empty for a display of no size, which is a panel the shell has not been told
/// the shape of yet. A menu with nothing to offer shows the one row that is
/// always true and no list at all.
pub fn offered(display: [u32; 2]) -> Vec<[u32; 2]> {
    let [width, height] = display;
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let mut sizes: Vec<[u32; 2]> = LADDER
        .iter()
        .copied()
        .filter(|size| fits(*size, display))
        .collect();
    for (numerator, denominator) in FRACTIONS.iter().copied() {
        // Whole in both directions or not at all: a fraction that has been
        // rounded is no longer the display's shape, which is the one promise
        // every row here makes.
        if width % denominator != 0 || height % denominator != 0 {
            continue;
        }
        let size = [
            width / denominator * numerator,
            height / denominator * numerator,
        ];
        if fits(size, display) && !sizes.contains(&size) {
            sizes.push(size);
        }
    }
    // Largest first, which is the order somebody reads a list of resolutions
    // in and the order the ladder itself is written in. By area, because two
    // sizes of the same shape cannot cross: whichever is wider is also taller.
    sizes.sort_by_key(|[width, height]| std::cmp::Reverse(*width as u64 * *height as u64));
    sizes
}

/// Whether one size is worth offering on a display this size: exactly its
/// shape, and smaller than it.
///
/// The shape test is a cross-multiplication rather than a comparison of two
/// divisions, so there is no tolerance to choose and no rounding to be wrong
/// about. `u64` because a 4K width times a 4K height is comfortably past what
/// a `u32` holds, and a wrapped product would make two unrelated shapes agree.
///
/// Smaller in width alone is the whole of the size test. Two sizes of the same
/// shape cannot be narrower and taller, so a row that is narrower is smaller in
/// both directions — and a row the same width is the display's own size, which
/// is not an alternative to itself.
pub fn fits(size: [u32; 2], display: [u32; 2]) -> bool {
    let ([width, height], [across, down]) = (size, display);
    width > 0
        && height > 0
        && width < across
        && width as u64 * down as u64 == height as u64 * across as u64
}

/// What one size is called on the menu: the two numbers, as the Display page
/// already writes a resolution.
///
/// Not translated, and there is nothing in it to translate — it is two numbers
/// and the sign between them, and the sign is the same one in every language
/// this shell is read in. *Native* is the row that does need a word, and it is
/// the one row this does not write.
pub fn label(size: [u32; 2]) -> String {
    format!("{} × {}", size[0], size[1])
}

/// What a resolution is being chosen for.
///
/// Three kinds of row on this bar carry the setting and only two kinds of key
/// are needed for them, which is what this enum is: an installed application
/// and a Steam title are both things with a name of their own, and one of the
/// user's own games is a file played by a program that has the same name
/// whichever game it is playing.
///
/// Both halves carry `names` — every name the compositor might see the windows
/// call themselves by — because that is what is *sent*, and it is not the same
/// as what is *filed*. A desktop entry says what its windows will be called and
/// is sometimes wrong; the shell sends every candidate rather than guessing,
/// and the compositor compares them whole. See [`crate::apps::App::window_names`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// Something with a name of its own: an installed application, or a Steam
    /// title, whose windows call themselves `steam_app_<id>` every time.
    Application {
        /// The name the answer is filed under — the first and best of `names`.
        key: String,
        names: Vec<String>,
    },
    /// One of the user's own games, filed under the file it is.
    ///
    /// The names here are the emulator's, not the game's, and they are why a
    /// game's answer has to be sent on the way into its own launch rather than
    /// once at startup: every game in the folder is played under them, so the
    /// last thing said about the emulator is what the next game gets. See
    /// `Shell::push_resolution_for_a_launch`.
    Game {
        path: std::path::PathBuf,
        names: Vec<String>,
    },
}

impl Subject {
    /// Every name the compositor might see this thing's windows use.
    pub fn names(&self) -> &[String] {
        match self {
            Self::Application { names, .. } | Self::Game { names, .. } => names,
        }
    }

    /// What has been chosen for it, or `None` for the display's own size.
    pub fn chosen(&self, prefs: &crate::pointer::Prefs) -> Option<[u32; 2]> {
        match self {
            Self::Application { key, .. } => prefs.resolution(key),
            Self::Game { path, .. } => prefs.game_resolution(path),
        }
    }

    /// Write a choice down. `true` when it is a change, which is what the
    /// caller tells the compositor and redraws on.
    pub fn choose(&self, prefs: &mut crate::pointer::Prefs, size: Option<[u32; 2]>) -> bool {
        match self {
            Self::Application { key, .. } => prefs.set_resolution(key, size),
            Self::Game { path, .. } => prefs.set_game_resolution(path, size),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The promise every offered row makes. A size that is not exactly the
    /// display's shape is one the compositor would fit inside the screen and
    /// leave a border round, which is a thing to avoid rather than a thing to
    /// put on a menu.
    #[test]
    fn every_size_offered_is_exactly_the_shape_of_the_screen() {
        for display in [
            [1920, 1080],
            [3840, 2160],
            [2560, 1440],
            [2560, 1600],
            [1920, 1200],
            [1600, 1200],
            [3440, 1440],
            [1080, 1920],
            [1280, 800],
        ] {
            for size in offered(display) {
                assert_eq!(
                    size[0] as u64 * display[1] as u64,
                    size[1] as u64 * display[0] as u64,
                    "{size:?} is not the shape of {display:?}"
                );
                assert!(
                    size[0] < display[0] && size[1] < display[1],
                    "{size:?} is not smaller than {display:?}"
                );
            }
        }
    }

    /// The familiar ones are what a familiar screen offers. A 1080p television
    /// asking for 720p is the whole reason this setting exists, and a list that
    /// answered with three quarters of 1080p instead would be right and useless.
    #[test]
    fn a_1080p_screen_offers_the_sizes_people_have_names_for() {
        let sizes = offered([1920, 1080]);
        for wanted in [[1600, 900], [1280, 720], [1024, 576], [960, 540]] {
            assert!(
                sizes.contains(&wanted),
                "{wanted:?} is missing from {sizes:?}"
            );
        }
        assert!(
            !sizes.contains(&[1920, 1080]),
            "the screen's own size is not an alternative to itself"
        );
        assert!(
            !sizes.contains(&[1280, 800]),
            "a 16∶10 size on a 16∶9 screen: {sizes:?}"
        );
    }

    /// And a screen no ladder anticipates still gets something. An ultrawide is
    /// the ordinary case of this, and the fractions are what answer it.
    #[test]
    fn a_screen_no_ladder_anticipates_still_gets_a_list() {
        let sizes = offered([2560, 1080]);
        assert!(!sizes.is_empty(), "nothing offered on an ultrawide");
        assert!(sizes.contains(&[1920, 810]), "{sizes:?}");
        assert!(
            sizes.contains(&[1280, 540]),
            "the half of it is missing: {sizes:?}"
        );
    }

    /// Largest first, which is the order a list of resolutions is read in.
    #[test]
    fn the_list_descends() {
        for display in [[1920, 1080], [3840, 2160], [2560, 1600], [3440, 1440]] {
            let sizes = offered(display);
            for pair in sizes.windows(2) {
                assert!(
                    pair[0][0] > pair[1][0],
                    "{:?} does not come before {:?}",
                    pair[0],
                    pair[1]
                );
            }
        }
    }

    /// A panel the shell has not been told the shape of has nothing to offer,
    /// and answering with a list would be answering about a screen nobody knows
    /// the size of.
    #[test]
    fn a_display_of_no_size_offers_nothing() {
        assert!(offered([0, 0]).is_empty());
        assert!(offered([1920, 0]).is_empty());
        assert!(offered([0, 1080]).is_empty());
    }

    /// Every rung of the ladder is exactly one of the shapes screens are sold
    /// in. One that is a pixel off — 1366×768 is the famous one — would never
    /// be offered on the screen it is named after, which is a row that exists
    /// and can never be reached.
    #[test]
    fn every_rung_of_the_ladder_is_the_shape_it_claims() {
        for size in LADDER {
            let doubled = [size[0] * 2, size[1] * 2];
            assert!(
                fits(*size, doubled),
                "{size:?} is not the shape of twice itself"
            );
        }
    }
}
