//! Icon theme lookup and decoding.
//!
//! Implements the parts of the freedesktop icon theme specification that a
//! launcher actually needs: search the current theme, follow `Inherits` from
//! `index.theme`, fall back to hicolor and then `/usr/share/pixmaps`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const ICON_EXTENSIONS: [&str; 3] = ["svg", "png", "xpm"];
const MAX_THEME_DEPTH: usize = 8;

/// Names for the glyphs the shell carries itself.
///
/// A colon, which no icon theme uses, so a built-in can never be shadowed by
/// an application that happens to install an icon of the same name.
pub const VOLUME: &str = "lxb:volume";
pub const VOLUME_MUTED: &str = "lxb:volume-muted";
pub const BRIGHTNESS: &str = "lxb:brightness";
/// The four tiles above the quick-settings bars: driving the pointer from the
/// right stick, the per-application volume mixer, whether anything is allowed
/// to interrupt, and what has been announced to the session while the user was
/// elsewhere.
///
/// Every glyph in the guide is lit by the same lamp above it, but these four
/// carry the most of that modelling: a gloss boundary that curves with the
/// object, shadow cast from one part onto the next, and controls sunk into
/// wells. They can afford it because they are the largest of the set — a tile
/// gives its glyph 42 px where a quick-settings bar gives 33 and the keyboard
/// hint 27, and a seam or a well drawn at 27 px is three grey pixels of
/// smudge. How much of the modelling each glyph keeps is a question of the
/// size it is drawn at and not of style; see brightness.svg for the reduction.
///
/// [`DO_NOT_DISTURB`] is the one mark in the shell that is knowingly a second
/// drawing of an object already in the set — [`SETTING_NIGHT_LIGHT`] is a moon
/// too. See do-not-disturb.svg for what keeps the two apart and why the rule
/// was bent for this one.
pub const POINTER_STICK: &str = "lxb:pointer-stick";
pub const VOLUME_MIXER: &str = "lxb:volume-mixer";
pub const DO_NOT_DISTURB: &str = "lxb:do-not-disturb";
pub const NOTIFICATIONS: &str = "lxb:notifications";
/// The two controller buttons the keyboard hint names. Drawn by position
/// rather than by letter, because A/B/X/Y are swapped between Xbox and
/// Nintendo pads and mean nothing at all on a PlayStation one, and Select is
/// branded Back, View, Share, Create or `−` depending on whose pad it is.
pub const PAD_SELECT: &str = "lxb:pad-select";
pub const PAD_WEST: &str = "lxb:pad-west";
/// The four arrow keys of the on-screen keyboard.
///
/// Drawn rather than lettered because Roboto — which the shell bundles so it
/// does not depend on what fonts a console has — carries no arrow glyphs, and
/// a keycap reading "Left" is not an arrow key.
pub const ARROW_LEFT: &str = "lxb:arrow-left";
pub const ARROW_DOWN: &str = "lxb:arrow-down";
pub const ARROW_UP: &str = "lxb:arrow-up";
pub const ARROW_RIGHT: &str = "lxb:arrow-right";
/// The on-screen keyboard's way out, drawn rather than lettered for the same
/// reason: it is the one key that has to be found without reading the board.
pub const KEYBOARD_HIDE: &str = "lxb:keyboard-hide";

/// One per column of the start screen's category row, in that row's order.
///
/// These are built in rather than looked up, which is a change from taking
/// `applications-graphics` and friends out of the icon theme. Three reasons,
/// and the first is the same one the quick-settings bars have: a machine with
/// no desktop on it has no theme to take them from, and an unlabelled row of
/// missing-icon squares is the one part of this shell that cannot degrade —
/// the row is the map of where everything lives.
///
/// The second is that a theme's application icons are *coloured*, and the
/// atlas multiplies the quad's colour into the texel. A coloured icon can only
/// come out muddier than the white it is tinted with, so the row ended up as a
/// line of dim blue and orange discs beside a shell drawn entirely in glass.
///
/// The third is that they were eleven icons from however many hands drew them.
/// These are eleven objects under one lamp, which is what a row is supposed to
/// look like.
pub const CATEGORY_SETTINGS: &str = "lxb:category-settings";
pub const CATEGORY_SYSTEM: &str = "lxb:category-system";
pub const CATEGORY_MULTIMEDIA: &str = "lxb:category-multimedia";
pub const CATEGORY_GRAPHICS: &str = "lxb:category-graphics";
pub const CATEGORY_INTERNET: &str = "lxb:category-internet";
pub const CATEGORY_OFFICE: &str = "lxb:category-office";
pub const CATEGORY_GAMES: &str = "lxb:category-games";
pub const CATEGORY_DEVELOPMENT: &str = "lxb:category-development";
pub const CATEGORY_EDUCATION: &str = "lxb:category-education";
pub const CATEGORY_UTILITIES: &str = "lxb:category-utilities";
pub const CATEGORY_OTHER: &str = "lxb:category-other";

/// The subcategories a column carries, which are rows inside a column rather
/// than columns of the row above: Multimedia's two, and the one under
/// Graphics.
///
/// Drawn to the same standard all the same, and for a reason the theme cannot
/// help with either: a subcategory that fell back to the missing-icon square
/// would read as an application that will not start, which is the one thing a
/// way further in must never look like.
///
/// Each is also the mark of the *files* inside it, since what they hold is the
/// user's own music, films and photographs rather than applications — a track
/// with no cover art was drawn as the mark of its column on the console too.
pub const CATEGORY_MUSIC: &str = "lxb:category-music";
pub const CATEGORY_VIDEO: &str = "lxb:category-video";
pub const CATEGORY_IMAGES: &str = "lxb:category-images";
/// System's own subcategory: the disks, walked folder by folder.
///
/// Two folders rather than one, which is the same distinction [`CATEGORY_IMAGES`]
/// draws against `CATEGORY_GRAPHICS` — the row is about a quantity of files
/// rather than about a file — and it is deliberately the same distinction so
/// that the bar has one way of saying it rather than three.
pub const CATEGORY_FILES: &str = "lxb:category-files";

/// The marks the file explorer's own rows wear: a folder, a file, a volume,
/// and the user's own folder at the head of it all.
///
/// Four objects rather than a table of one per file type. What a `.pdf` is, is
/// written on the row in words the user can read; a cabinet of half-recognised
/// drawings under that would be the same information told worse, and told
/// wrongly the moment somebody keeps a format nothing has heard of. Where the
/// shell *does* already know a file — a song, a film, a photograph — it keeps
/// that shelf's own mark instead, so one file has one drawing wherever it is
/// being looked at from. See file-page.svg.
pub const FILE_FOLDER: &str = "lxb:file-folder";
pub const FILE_PAGE: &str = "lxb:file-page";
pub const FILE_DRIVE: &str = "lxb:file-drive";
pub const FILE_HOME: &str = "lxb:file-home";

/// The Steam column, and the mark every row that came from Steam wears.
///
/// Two drawings of one object rather than one drawing used twice, because the
/// two are asked different questions — see the files themselves. Built in like
/// the rest of the row: the column exists only while somebody is signed in,
/// and a machine with no icon theme must still be able to draw the column that
/// signing in produced.
///
/// Drawn here rather than taken from the Steam client's own icon for the
/// reason every other column glyph is: a coloured application icon comes out
/// muddy through an atlas that tints what it samples, and this shell's own
/// hand is what makes eleven columns look like one row.
pub const CATEGORY_STEAM: &str = "lxb:category-steam";
pub const STEAM: &str = "lxb:steam";

/// The two rows of the menu raised over the Steam entry itself, which are
/// about the *account* rather than about anything in its library: asking for
/// the library again, and leaving.
///
/// [`SIGN_OUT`] exists because the row wore [`UNINSTALL`] before it, and a
/// waste bin over Sign out says the wrong thing twice — nothing is removed, and
/// the account is still there to sign back into. [`REFRESH`] because the row
/// had no mark at all, which left the only two rows on that panel unable to
/// line up with one another.
///
/// Deliberately unlike [`SETTING_REFRESH`], the display's refresh rate: that is
/// one ring redrawing itself inside a monitor, and this is two arrows chasing
/// each other with no monitor anywhere near them.
pub const REFRESH: &str = "lxb:refresh";
pub const SIGN_OUT: &str = "lxb:sign-out";

/// The shell's own rows in the Settings column, and the two marks a list of
/// values is made of.
///
/// Built in for the same reason the category row is: these are LineXinBar's own
/// settings, and a shell whose settings came up as blank squares on a machine
/// with no icon theme installed would be missing the one column it is entirely
/// responsible for. [`SWATCH`] is drawn white on purpose — the atlas
/// multiplies a quad's colour into the texel, so one drawing serves every
/// colour a list of them can hold.
pub const SETTING_APPEARANCE: &str = "lxb:setting-appearance";
pub const SETTING_ACCENT: &str = "lxb:setting-accent";
pub const SETTING_DISPLAY: &str = "lxb:setting-display";
pub const SETTING_RESOLUTION: &str = "lxb:setting-resolution";
pub const SETTING_REFRESH: &str = "lxb:setting-refresh";
pub const SETTING_ORIENTATION: &str = "lxb:setting-orientation";
pub const SETTING_HDR: &str = "lxb:setting-hdr";

/// Settings > Display > Display order: which screen the compositor puts first.
///
/// The one glyph in this set with two screens in it, because it is the one page
/// under Display that is about the screens rather than about a screen — see the
/// file itself. It goes on that folder and nowhere else; the screens listed
/// inside it wear [`SETTING_DISPLAY`], as they do on every other page there.
pub const SETTING_ORDER: &str = "lxb:setting-order";

/// Settings > Display > Night light: the blue light filter, and the hours it
/// keeps.
///
/// [`SETTING_NIGHT_LIGHT`] is a crescent moon, drawn against [`BRIGHTNESS`]'s
/// sun on purpose — the same ball with the light taken off most of it. It goes
/// on the Night light folder and on the switch inside it, and nowhere else, the
/// way [`SETTING_HDR`] is kept to HDR.
///
/// [`SETTING_SCHEDULE`] is a clock face, worn by both hour rows: they are two
/// ends of one thing and the shell has one picture of a time. It is not the
/// moon, because the row above them already says which setting these hours
/// belong to and repeating it there would leave three identical marks down one
/// column.
pub const SETTING_NIGHT_LIGHT: &str = "lxb:setting-night-light";
pub const SETTING_SCHEDULE: &str = "lxb:setting-schedule";
/// The device everything on the machine records from, under Settings > Sounds.
///
/// The one row of that page with a drawing of its own rather than a borrowed
/// one: the output beside it wears [`VOLUME`], which is the speaker the volume
/// bar wears, because a second speaker drawn for this set could only be that
/// same speaker again or a worse one. There is no microphone anywhere else in
/// the shell to borrow, and an input device drawn as a speaker would be saying
/// the wrong thing rather than repeating a right one.
pub const SETTING_MICROPHONE: &str = "lxb:setting-microphone";

/// The four turns under Settings > Display > Orientation, drawn as what they
/// are: one monitor, stood four ways, its stand saying which way up.
///
/// Values rather than subcategories, and the only values in the Settings tree
/// with drawings of their own — everything else there is a colour, a number or
/// a switch, and wears the bead [`SWATCH`] instead. These earn the exception
/// because what is being chosen *is* a shape: the row that matches the screen
/// in front of the user can be picked without reading it.
pub const SETTING_ROTATION_0: &str = "lxb:setting-rotation-0";
pub const SETTING_ROTATION_90: &str = "lxb:setting-rotation-90";
pub const SETTING_ROTATION_180: &str = "lxb:setting-rotation-180";
pub const SETTING_ROTATION_270: &str = "lxb:setting-rotation-270";
/// Settings > System, and the one row under it: how large applications draw
/// themselves.
///
/// [`SETTING_SYSTEM`] is a chip. It is neither the cog that stands for the whole
/// Settings column nor another monitor — every drawing of one in this set
/// belongs to a page under Display — because this is the page about the machine
/// rather than about its picture. See setting-system.svg.
///
/// [`SETTING_SCALE`] is a window and the larger window it is being drawn out to,
/// with one arrow along the diagonal. Deliberately unlike
/// [`SETTING_RESOLUTION`], which measures a *screen's* pixels across and down
/// inside a bezel: this has no bezel and one arrow, because what it changes is
/// one number that moves both dimensions at once, and it changes it for
/// applications rather than for the display.
pub const SETTING_SYSTEM: &str = "lxb:setting-system";
pub const SETTING_SCALE: &str = "lxb:setting-scale";

/// A read-only explanation in Settings, visually distinct from the control it
/// sits beneath so an unavailable mode or capability is not mistaken for HDR.
pub const SETTING_INFO: &str = "lxb:setting-info";
pub const SWATCH: &str = "lxb:swatch";
pub const CHOSEN: &str = "lxb:chosen";

/// The two context-menu rows that act on the application itself: removing it
/// from the machine, and starting it.
///
/// Built in like the rest, and drawn rather than taken from the icon theme for
/// the third reason the category row gives: `edit-delete` and
/// `media-playback-start` come from however many hands drew whatever theme is
/// installed, and the menu they sit in has one lamp over it.
pub const UNINSTALL: &str = "lxb:uninstall";
pub const LAUNCH: &str = "lxb:launch";

/// The two rows of the menu over one of the user's own files that lead onward
/// rather than doing something: choosing which application opens it, and
/// choosing what order the column is listed in.
///
/// Drawn rather than themed for the same reason as the two above, and drawn
/// unlike each other on purpose: they sit four rows apart in one panel, and two
/// marks that both said "there is more this way" would leave the panel with two
/// rows the eye cannot tell apart.
pub const OPEN_WITH: &str = "lxb:open-with";
pub const SORT: &str = "lxb:sort";

/// The two rows at the head of a column of the user's own files: the field
/// that searches it, and the row that empties the field.
///
/// Built in for the same reason as everything above, and for one more that is
/// particular to them. These sit *in a column*, beside a row drawn with a
/// photograph out of the user's own collection and a row drawn with a note —
/// so they have to be of the same material as the rest of the shell, and an
/// icon theme's flat `edit-find` beside a glossy note would be the seam
/// showing.
pub const SEARCH: &str = "lxb:search";
pub const SEARCH_CLEAR: &str = "lxb:search-clear";

/// The guide menu's Screenshot row.
///
/// Built in like the two above, and for the third reason again: `camera-photo`
/// is a coloured icon from whichever theme is installed, and the row it sits in
/// has one lamp over it and two arrows drawn under that lamp already.
pub const SCREENSHOT: &str = "lxb:screenshot";

/// The panel that asks the user to prove they may do something — see
/// [`crate::polkit`].
///
/// Built in, and here rather than taken from the icon theme for a reason none
/// of the others have. polkit hands the agent an icon *name* with every
/// question, chosen by whoever wrote the policy file, and it names an icon out
/// of whatever theme happens to be installed. Two things are wrong with using
/// it: the shell stands an application icon in for any name it cannot find, so
/// a panel asking for a password could come up wearing the generic executable
/// mark, and a themed icon on the one panel in the shell that takes a password
/// is a picture chosen by the program doing the asking. One padlock, always the
/// same one, is the honest answer.
pub const AUTHENTICATE: &str = "lxb:authenticate";

/// The power button at the foot of the guide's sidebar.
///
/// The shell used to assemble this out of two solid quads — a ring with a
/// notch cut in it and a rod above — which was the last flat mark left in that
/// column. `ui::power_glyph` still exists and still draws it, as the fallback
/// for a glyph that somehow failed to rasterise: of everything in the sidebar
/// this is the one button that must never come up empty, because it has no
/// label to fall back on.
pub const SHUTDOWN: &str = "lxb:shutdown";

/// Every built-in, as `(name, drawing)`, for the atlas to load at startup.
///
/// Compiled into the binary from files in the tree, the way the font and the
/// shaders are. Two things follow from that, and both are the point: the shell
/// has these whatever is installed on the machine — which is what the
/// quick-settings bars are *for* — and they are still drawings, editable in
/// anything that opens an SVG rather than in a string literal.
pub const BUILTIN: [(&str, &str); 66] = [
    (VOLUME, include_str!("glyphs/volume.svg")),
    (VOLUME_MUTED, include_str!("glyphs/volume-muted.svg")),
    (BRIGHTNESS, include_str!("glyphs/brightness.svg")),
    (POINTER_STICK, include_str!("glyphs/pointer-stick.svg")),
    (VOLUME_MIXER, include_str!("glyphs/volume-mixer.svg")),
    (DO_NOT_DISTURB, include_str!("glyphs/do-not-disturb.svg")),
    (NOTIFICATIONS, include_str!("glyphs/notifications.svg")),
    (PAD_SELECT, include_str!("glyphs/pad-select.svg")),
    (PAD_WEST, include_str!("glyphs/pad-west.svg")),
    (ARROW_LEFT, include_str!("glyphs/arrow-left.svg")),
    (ARROW_DOWN, include_str!("glyphs/arrow-down.svg")),
    (ARROW_UP, include_str!("glyphs/arrow-up.svg")),
    (ARROW_RIGHT, include_str!("glyphs/arrow-right.svg")),
    (KEYBOARD_HIDE, include_str!("glyphs/keyboard-hide.svg")),
    (SHUTDOWN, include_str!("glyphs/shutdown.svg")),
    // The category row, in the order it is laid out in.
    (
        CATEGORY_SETTINGS,
        include_str!("glyphs/category-settings.svg"),
    ),
    (CATEGORY_SYSTEM, include_str!("glyphs/category-system.svg")),
    (
        CATEGORY_MULTIMEDIA,
        include_str!("glyphs/category-multimedia.svg"),
    ),
    (
        CATEGORY_GRAPHICS,
        include_str!("glyphs/category-graphics.svg"),
    ),
    (
        CATEGORY_INTERNET,
        include_str!("glyphs/category-internet.svg"),
    ),
    (CATEGORY_OFFICE, include_str!("glyphs/category-office.svg")),
    (CATEGORY_GAMES, include_str!("glyphs/category-games.svg")),
    (
        CATEGORY_DEVELOPMENT,
        include_str!("glyphs/category-development.svg"),
    ),
    (
        CATEGORY_EDUCATION,
        include_str!("glyphs/category-education.svg"),
    ),
    (
        CATEGORY_UTILITIES,
        include_str!("glyphs/category-utilities.svg"),
    ),
    (CATEGORY_OTHER, include_str!("glyphs/category-other.svg")),
    // Then the rows that stand inside a column rather than along the row.
    (CATEGORY_MUSIC, include_str!("glyphs/category-music.svg")),
    (CATEGORY_VIDEO, include_str!("glyphs/category-video.svg")),
    (CATEGORY_IMAGES, include_str!("glyphs/category-images.svg")),
    (CATEGORY_FILES, include_str!("glyphs/category-files.svg")),
    // And the rows the file explorer under it is made of.
    (FILE_FOLDER, include_str!("glyphs/file-folder.svg")),
    (FILE_PAGE, include_str!("glyphs/file-page.svg")),
    (FILE_DRIVE, include_str!("glyphs/file-drive.svg")),
    (FILE_HOME, include_str!("glyphs/file-home.svg")),
    // The Settings column's own rows, and the marks its lists are made of.
    (
        SETTING_APPEARANCE,
        include_str!("glyphs/setting-appearance.svg"),
    ),
    (SETTING_ACCENT, include_str!("glyphs/setting-accent.svg")),
    (SETTING_DISPLAY, include_str!("glyphs/setting-display.svg")),
    (
        SETTING_RESOLUTION,
        include_str!("glyphs/setting-resolution.svg"),
    ),
    (SETTING_REFRESH, include_str!("glyphs/setting-refresh.svg")),
    (
        SETTING_ORIENTATION,
        include_str!("glyphs/setting-orientation.svg"),
    ),
    (
        SETTING_ROTATION_0,
        include_str!("glyphs/setting-rotation-0.svg"),
    ),
    (
        SETTING_ROTATION_90,
        include_str!("glyphs/setting-rotation-90.svg"),
    ),
    (
        SETTING_ROTATION_180,
        include_str!("glyphs/setting-rotation-180.svg"),
    ),
    (
        SETTING_ROTATION_270,
        include_str!("glyphs/setting-rotation-270.svg"),
    ),
    (SETTING_ORDER, include_str!("glyphs/setting-order.svg")),
    (
        SETTING_NIGHT_LIGHT,
        include_str!("glyphs/setting-night-light.svg"),
    ),
    (
        SETTING_SCHEDULE,
        include_str!("glyphs/setting-schedule.svg"),
    ),
    (SETTING_HDR, include_str!("glyphs/setting-hdr.svg")),
    (
        SETTING_MICROPHONE,
        include_str!("glyphs/setting-microphone.svg"),
    ),
    (SETTING_SYSTEM, include_str!("glyphs/setting-system.svg")),
    (SETTING_SCALE, include_str!("glyphs/setting-scale.svg")),
    (SETTING_INFO, include_str!("glyphs/setting-info.svg")),
    (SWATCH, include_str!("glyphs/swatch.svg")),
    (CHOSEN, include_str!("glyphs/chosen.svg")),
    // The context menu's own rows.
    (UNINSTALL, include_str!("glyphs/uninstall.svg")),
    (LAUNCH, include_str!("glyphs/launch.svg")),
    (OPEN_WITH, include_str!("glyphs/open-with.svg")),
    (SORT, include_str!("glyphs/sort.svg")),
    (SCREENSHOT, include_str!("glyphs/screenshot.svg")),
    // The rows a column of the user's own files carries above the files.
    (SEARCH, include_str!("glyphs/search.svg")),
    (SEARCH_CLEAR, include_str!("glyphs/search-clear.svg")),
    (AUTHENTICATE, include_str!("glyphs/authenticate.svg")),
    // The Steam column, and the rows that came out of it.
    (CATEGORY_STEAM, include_str!("glyphs/category-steam.svg")),
    (STEAM, include_str!("glyphs/steam.svg")),
    (REFRESH, include_str!("glyphs/refresh.svg")),
    (SIGN_OUT, include_str!("glyphs/sign-out.svg")),
];

/// A decoded icon, always square RGBA8.
pub struct Icon {
    pub size: u32,
    pub rgba: Vec<u8>,
}

impl Icon {
    /// Rasterise a drawing the shell carries itself.
    ///
    /// The glyphs on the quick-settings bars come through here rather than out
    /// of the icon theme. A theme is something a desktop installs, and the
    /// point of those bars is a session with no desktop in it — a speaker that
    /// is missing on a machine with only hicolor installed would leave the
    /// volume row unlabelled on exactly the systems this is for.
    pub fn builtin(svg: &str, size: u32) -> Option<Self> {
        Some(Icon {
            size,
            rgba: rasterise_svg(svg.as_bytes(), None, size)?,
        })
    }

    /// Build one out of pixels somebody handed over rather than out of a file.
    ///
    /// For the announcement that carries its picture as raw bytes — album art,
    /// a correspondent's face — which arrives as a block of samples and a
    /// description of how to read it, and never as anything nameable.
    ///
    /// `stride` is the distance from one row of that block to the next, which
    /// is not always the width: a sender is free to pad its rows, and reading
    /// the block as though it were tight is what turns a padded picture into a
    /// diagonal smear. `channels` is three or four, and three means every
    /// pixel is opaque.
    ///
    /// Scaled to `size` the same way a file is, so a picture of any shape ends
    /// up square without being stretched into one.
    pub fn from_pixels(
        width: u32,
        height: u32,
        stride: u32,
        channels: u32,
        data: &[u8],
        size: u32,
    ) -> Option<Self> {
        if width == 0 || height == 0 || size == 0 || !(3..=4).contains(&channels) {
            return None;
        }
        let row = width.checked_mul(channels)?;
        if stride < row {
            return None;
        }
        // The last row need only be as long as the picture is wide: a sender
        // that padded every row *between* its rows has no reason to pad past
        // the end, and refusing that would be refusing a valid picture.
        let needed = stride
            .checked_mul(height.checked_sub(1)?)?
            .checked_add(row)?;
        if data.len() < needed as usize {
            return None;
        }

        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            let start = (y * stride) as usize;
            for x in 0..width {
                let pixel = start + (x * channels) as usize;
                rgba.extend_from_slice(&data[pixel..pixel + 3]);
                rgba.push(if channels == 4 { data[pixel + 3] } else { 255 });
            }
        }

        let buffer = image::RgbaImage::from_raw(width, height, rgba)?;
        Some(Icon {
            size,
            rgba: fit_raster(image::DynamicImage::ImageRgba8(buffer), size),
        })
    }
}

pub struct IconLoader {
    /// Every directory that can hold an icon, in priority order, enumerated
    /// once at construction.
    ///
    /// The alternative — walking the theme tree per icon name — costs a couple
    /// of thousand `stat` calls each time, and a launcher resolves a hundred
    /// names before it can draw anything.
    search_dirs: Vec<(u32, PathBuf)>,
    cache: HashMap<String, Option<PathBuf>>,
}

impl IconLoader {
    pub fn new() -> Self {
        let roots = icon_roots();
        let theme = current_theme();
        let pixmaps = ["/usr/share/pixmaps", "/usr/local/share/pixmaps"]
            .into_iter()
            .map(PathBuf::from)
            .collect();
        Self::from_paths(roots, theme, pixmaps)
    }

    fn from_paths(roots: Vec<PathBuf>, theme: String, pixmap_dirs: Vec<PathBuf>) -> Self {
        let mut themes = Vec::new();
        collect_theme_chain(&roots, &theme, &mut themes, &mut HashSet::new());

        // hicolor is the mandated final fallback for every theme.
        if !themes.iter().any(|t| t == "hicolor") {
            themes.push("hicolor".to_string());
        }

        // Rank encodes the priority order: earlier themes beat later ones, and
        // within a theme a larger (or scalable) source beats a smaller one.
        let mut search_dirs: Vec<(u32, PathBuf)> = Vec::new();
        let mut theme_count = 0u32;
        let mut seen_dirs = HashSet::new();

        for (index, theme) in themes.iter().enumerate() {
            // Themes are strictly ordered, so keep their bands far apart.
            let theme_rank = (themes.len() - index) as u32 * 1_000_000;
            for root in &roots {
                let theme_dir = root.join(theme);
                if !theme_dir.is_dir() {
                    continue;
                }
                theme_count += 1;
                collect_icon_directories(
                    &theme_dir,
                    &theme_dir,
                    theme_rank,
                    0,
                    &mut seen_dirs,
                    &mut search_dirs,
                );
            }
        }

        // Pixmaps are the last resort, below every theme.
        for dir in pixmap_dirs {
            if dir.is_dir() {
                search_dirs.push((0, dir));
            }
        }

        // Highest rank first, so candidate resolution prefers it.
        search_dirs.sort_by_key(|entry| std::cmp::Reverse(entry.0));

        tracing::debug!(
            ?themes,
            themes_found = theme_count,
            dirs = search_dirs.len(),
            "icon search path"
        );

        Self {
            search_dirs,
            cache: HashMap::new(),
        }
    }

    /// Load and rasterise an icon to exactly `size` x `size` RGBA pixels.
    pub fn load(&mut self, name: &str, size: u32) -> Option<Icon> {
        if name.is_empty() || size == 0 {
            return None;
        }

        if matches!(self.cache.get(name), Some(None)) {
            return None;
        }

        let cached = self.cache.get(name).and_then(Clone::clone);
        let mut tried = HashSet::new();
        let candidates = cached
            .into_iter()
            .chain(self.candidates(name))
            .filter(|path| tried.insert(path.clone()));

        for path in candidates {
            if !path.is_file() {
                continue;
            }
            if let Some(icon) = load_path(&path, size) {
                self.cache.insert(name.to_string(), Some(path));
                return Some(icon);
            }
            // Do not let a broken SVG or unsupported XPM in the current theme
            // hide a valid PNG/SVG inherited from a lower-priority theme.
            tracing::debug!(icon = name, path = %path.display(), "could not decode icon; trying fallback");
        }

        self.cache.insert(name.to_string(), None);
        tracing::debug!(icon = name, "icon not found or no decodable fallback");
        None
    }

    fn candidates(&self, name: &str) -> Vec<PathBuf> {
        let path = Path::new(name);
        if path.is_absolute() {
            return vec![path.to_path_buf()];
        }

        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            return Vec::new();
        };
        let supplied_extension = known_icon_extension(path);
        let stem = supplied_extension
            .and_then(|_| path.file_stem())
            .and_then(|value| value.to_str())
            .unwrap_or(file_name);

        let mut candidates = Vec::new();
        for (_, dir) in &self.search_dirs {
            // An explicit recognised suffix gets first chance, but decoding
            // failure still falls through to the other supported formats.
            if supplied_extension.is_some() {
                candidates.push(dir.join(file_name));
            }
            for extension in ICON_EXTENSIONS {
                if supplied_extension == Some(extension) {
                    continue;
                }
                candidates.push(dir.join(format!("{stem}.{extension}")));
            }
        }
        candidates
    }
}

impl Default for IconLoader {
    fn default() -> Self {
        Self::new()
    }
}

fn load_path(path: &Path, size: u32) -> Option<Icon> {
    if size == 0 {
        return None;
    }

    // Read once and sniff the contents.  AppImage-generated desktop entries
    // commonly use absolute PNG/SVG paths with no filename extension.
    let data = std::fs::read(path).ok()?;
    let rgba = if let Some(svg) = rasterise_svg(&data, path.parent(), size) {
        svg
    } else {
        let image = image::load_from_memory(&data).ok()?;
        fit_raster(image, size)
    };
    Some(Icon { size, rgba })
}

pub fn rasterise_svg(data: &[u8], resources_dir: Option<&Path>, size: u32) -> Option<Vec<u8>> {
    use resvg::tiny_skia;
    use resvg::usvg;

    // resvg is built without its text feature: icons are shapes, and pulling in
    // a font database here would duplicate the one glyphon already owns.
    let options = usvg::Options {
        resources_dir: resources_dir.map(Path::to_path_buf),
        ..Default::default()
    };
    let tree = usvg::Tree::from_data(data, &options).ok()?;

    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let tree_size = tree.size();
    // Fit the drawing into the square without distorting it.
    let scale = (size as f32 / tree_size.width()).min(size as f32 / tree_size.height());
    let dx = (size as f32 - tree_size.width() * scale) / 2.0;
    let dy = (size as f32 - tree_size.height() * scale) / 2.0;

    let transform = tiny_skia::Transform::from_translate(dx, dy).pre_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    let mut rgba = pixmap.take();
    // tiny-skia stores premultiplied RGBA, while the atlas pipeline uses
    // straight-alpha blending.  Convert once here to avoid dark translucent
    // edges and double-multiplying SVG alpha in the shader pipeline.
    unpremultiply_rgba(&mut rgba);
    Some(rgba)
}

fn fit_raster(image: image::DynamicImage, size: u32) -> Vec<u8> {
    let resized = image
        .resize(size, size, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    if resized.width() == size && resized.height() == size {
        return resized.into_raw();
    }

    let mut square = image::RgbaImage::new(size, size);
    let x = (size - resized.width()) / 2;
    let y = (size - resized.height()) / 2;
    image::imageops::overlay(&mut square, &resized, i64::from(x), i64::from(y));
    square.into_raw()
}

fn unpremultiply_rgba(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        if alpha == 0 || alpha == 255 {
            continue;
        }
        for channel in &mut pixel[..3] {
            let straight = (u32::from(*channel) * 255 + alpha / 2) / alpha;
            *channel = straight.min(255) as u8;
        }
    }
}

fn known_icon_extension(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?;
    ICON_EXTENSIONS
        .into_iter()
        .find(|known| extension.eq_ignore_ascii_case(known))
}

/// Discover directories recursively instead of assuming either
/// `size/context` or `context/size`.  Both layouts are common in real themes,
/// and the specification permits arbitrary relative paths in `Directories=`.
fn collect_icon_directories(
    theme_root: &Path,
    directory: &Path,
    theme_rank: u32,
    depth: usize,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<(u32, PathBuf)>,
) {
    if depth > MAX_THEME_DEPTH {
        return;
    }

    let identity = directory
        .canonicalize()
        .unwrap_or_else(|_| directory.to_path_buf());
    if !seen.insert(identity) {
        return;
    }

    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut contains_icons = false;
    let mut children = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            children.push(path);
        } else if known_icon_extension(&path).is_some() {
            contains_icons = true;
        }
    }

    if contains_icons {
        let relative = directory.strip_prefix(theme_root).unwrap_or(directory);
        out.push((
            theme_rank + directory_rank(relative),
            directory.to_path_buf(),
        ));
    }
    for child in children {
        collect_icon_directories(theme_root, &child, theme_rank, depth + 1, seen, out);
    }
}

/// Rank a theme size directory so the closest-to-ideal resolution wins.
///
/// Scalable directories rank highest, then the largest fixed size.
fn directory_rank(path: &Path) -> u32 {
    let mut largest = 0;
    for component in path
        .components()
        .filter_map(|part| part.as_os_str().to_str())
    {
        if component.to_ascii_lowercase().contains("scalable") {
            return 100_000;
        }

        // Components are commonly `48x48`, `48`, or `48x48@2`.
        let dims = component.split('@').next().unwrap_or(component);
        let size = dims
            .split(['x', 'X'])
            .next()
            .and_then(|value| value.parse::<u32>().ok())
            .unwrap_or(0);
        largest = largest.max(size.min(1024));
    }
    // Prefer bigger sources; the decoder always scales down to the atlas cell.
    largest
}

fn icon_roots() -> Vec<PathBuf> {
    // `~/.icons` is legacy but still widely populated, so it comes first and
    // is not covered by the XDG data dirs.
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(&home).join(".icons"));
    }
    roots.extend(crate::xdg_data_dirs("icons"));

    roots.retain(|r| r.is_dir());
    let mut seen = HashSet::new();
    roots.retain(|root| seen.insert(root.clone()));
    roots
}

/// Best guess at the user's theme, since we have no settings daemon to ask.
fn current_theme() -> String {
    if let Ok(theme) = std::env::var("LXB_ICON_THEME") {
        if !theme.is_empty() {
            return theme;
        }
    }
    // KDE records it here; GTK has its own file. Reading them is cheap.
    //
    // Both, now. This said the same and read only KDE's, which meant a machine
    // configured through GTK alone — GNOME, XFCE, or a bare session where the
    // user has set a theme and nothing else — fell all the way back to
    // hicolor. That is survivable for application icons, because a program
    // installs its own into hicolor, and it is not survivable for the standard
    // names: `software-update-available` and its like come *from* a theme, and
    // hicolor is exactly the theme that does not carry them. An announcement
    // naming one got the bell.
    if let Some(theme) = read_kde_theme().or_else(read_gtk_theme) {
        return theme;
    }
    // And when neither desktop is here to have left a file — which is the
    // machine this shell is *for*, a console with no desktop on it at all —
    // the shell picks one itself rather than falling to hicolor.
    //
    // Falling to hicolor is not a neutral default, it is a broken one. hicolor
    // is the place a program installs its *own* icon, so an application still
    // has a picture there; what it has never carried is the standard names —
    // `software-update-available`, `dialog-warning`, `network-wireless` — and
    // those are exactly what a program names when it announces something. A
    // session with no desktop would have had a bell on every announcement,
    // with a perfectly good theme sitting installed on the disk unread.
    //
    // Only when nothing has been configured, so a user who has said what they
    // want is never second-guessed, and it costs nothing on a machine that has
    // said: this does not run at all until both files have come back empty.
    if let Some(theme) = any_installed_theme(&icon_roots()) {
        tracing::info!(theme, "no desktop has named an icon theme; using this one");
        return theme;
    }
    "hicolor".to_string()
}

/// The themes most likely to be both installed and complete, in the order they
/// are worth trying.
///
/// The two that ship with the two toolkits: a machine with any GTK application
/// on it usually has Adwaita, and one with any Qt application usually has
/// Breeze. Papirus after them because it is the one people install on purpose.
///
/// A list of names is a blunt instrument and it is the honest one here. The
/// alternative is to rank what is installed by how many icons it carries,
/// which means walking every theme on the disk to answer a question asked once
/// per session, and still gets it wrong — the biggest theme is not the most
/// complete one, it is the one with the most sizes.
const LIKELY_THEMES: [&str; 3] = ["Adwaita", "breeze", "Papirus"];

/// A theme that is actually on this machine, when nothing has said which to
/// use.
///
/// Anything with an `index.theme` that lists directories. That last part is
/// what keeps cursor themes out: a cursor theme is an icon theme by file
/// layout — same place on disk, same index file — and carries no icons at all,
/// so a session that picked one would be back to having none.
fn any_installed_theme(roots: &[PathBuf]) -> Option<String> {
    let mut found: Vec<String> = Vec::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name == "hicolor" || found.contains(&name) {
                continue;
            }
            if read_ini_key(
                &entry.path().join("index.theme"),
                Some("[Icon Theme]"),
                "Directories",
            )
            .is_some()
            {
                found.push(name);
            }
        }
    }

    LIKELY_THEMES
        .into_iter()
        .find(|likely| found.iter().any(|name| name == likely))
        .map(str::to_string)
        // Failing all of those, whatever is here — sorted, so that two
        // sessions on one machine make the same choice and the shell does not
        // change its look because a directory was read back in another order.
        .or_else(|| {
            found.sort();
            found.into_iter().next()
        })
}

fn read_kde_theme() -> Option<String> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    read_ini_key(&home.join(".config/kdeglobals"), Some("[Icons]"), "Theme")
}

/// GTK 4 first and then GTK 3, so a machine that has moved on is read as it is
/// now rather than as it was.
fn read_gtk_theme() -> Option<String> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    ["gtk-4.0", "gtk-3.0"].into_iter().find_map(|version| {
        read_ini_key(
            &home.join(format!(".config/{version}/settings.ini")),
            Some("[Settings]"),
            "gtk-icon-theme-name",
        )
    })
}

/// One `Key=Value` out of a desktop-style ini file.
///
/// `section` is the header the key has to be under, or `None` for a file with
/// no sections. GTK's own `settings.ini` is supposed to have a `[Settings]`
/// header and is routinely written without one, so a file whose first line is
/// already a key is read as though the header it was missing had been there —
/// anything else would be refusing to read a file the toolkit that owns it
/// reads happily.
///
/// Takes the file rather than finding it, so that what it does can be tested
/// without a test setting `HOME` — an environment variable is one per process,
/// and tests here run beside each other.
fn read_ini_key(path: &Path, section: Option<&str>, key: &str) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;

    let mut inside = section.is_none();
    let mut seen_any_section = false;
    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            seen_any_section = true;
            inside = section.is_none_or(|wanted| line == wanted);
            continue;
        }
        if !inside && seen_any_section {
            continue;
        }
        if let Some(value) = line.strip_prefix(key).and_then(|rest| {
            // The key and nothing longer that starts with it, and the equals
            // sign may have spaces round it as GTK writes them.
            rest.trim_start().strip_prefix('=')
        }) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Walk `Inherits=` chains so a theme's fallbacks are searched too.
fn collect_theme_chain(
    roots: &[PathBuf],
    theme: &str,
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    if !seen.insert(theme.to_string()) {
        return;
    }
    out.push(theme.to_string());

    for root in roots {
        let index = root.join(theme).join("index.theme");
        let Ok(raw) = std::fs::read_to_string(&index) else {
            continue;
        };
        for line in raw.lines() {
            let line = line.trim();
            if let Some(value) = line.strip_prefix("Inherits=") {
                for parent in value.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    collect_theme_chain(roots, parent, out, seen);
                }
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestTree(PathBuf);

    impl TestTree {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "lxb-icon-test-{label}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn join(&self, path: impl AsRef<Path>) -> PathBuf {
            self.0.join(path)
        }
    }

    impl Drop for TestTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn png_bytes(width: u32, height: u32, color: [u8; 4]) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba(color));
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    fn write(path: &Path, bytes: impl AsRef<[u8]>) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    fn loader_with_dirs(dirs: Vec<PathBuf>) -> IconLoader {
        IconLoader {
            search_dirs: dirs
                .into_iter()
                .enumerate()
                .map(|(index, path)| (10_000 - index as u32, path))
                .collect(),
            cache: HashMap::new(),
        }
    }

    /// The icon theme is read from whichever file the machine happens to keep
    /// it in, and read the way the toolkits that own those files write them.
    ///
    /// This decides whether a *name* resolves to anything at all. Only KDE's
    /// file was read, so a machine configured through GTK alone fell back to
    /// hicolor — survivable for application icons, which programs install into
    /// hicolor themselves, and not survivable for the standard names an
    /// announcement gives, which come from a theme and are the one thing
    /// hicolor does not carry.
    #[test]
    fn the_icon_theme_is_read_from_either_desktops_file() {
        let tree = TestTree::new("theme");

        let kde = tree.join("kdeglobals");
        write(
            &kde,
            "[General]\nTheme=NotThisOne\n\n[Icons]\nTheme=Tela-dark\n",
        );
        assert_eq!(
            read_ini_key(&kde, Some("[Icons]"), "Theme").as_deref(),
            Some("Tela-dark"),
            "the key under the section that was asked for, not the first that matches"
        );

        // GTK writes spaces round the equals and does not always write the
        // header at all.
        let gtk = tree.join("settings.ini");
        write(&gtk, "[Settings]\ngtk-icon-theme-name = Papirus\n");
        assert_eq!(
            read_ini_key(&gtk, Some("[Settings]"), "gtk-icon-theme-name").as_deref(),
            Some("Papirus")
        );
        write(&gtk, "gtk-icon-theme-name=Papirus\n");
        assert_eq!(
            read_ini_key(&gtk, Some("[Settings]"), "gtk-icon-theme-name").as_deref(),
            Some("Papirus"),
            "a headerless file is read the way GTK itself reads it"
        );

        // A key that merely starts with the one wanted is a different key.
        write(&gtk, "[Settings]\ngtk-icon-theme-name-fallback=Wrong\n");
        assert_eq!(
            read_ini_key(&gtk, Some("[Settings]"), "gtk-icon-theme-name"),
            None
        );

        // And nothing is not something: an empty value leaves the search where
        // it was rather than naming a theme called "".
        write(&gtk, "[Settings]\ngtk-icon-theme-name=\n");
        assert_eq!(
            read_ini_key(&gtk, Some("[Settings]"), "gtk-icon-theme-name"),
            None
        );
        assert_eq!(
            read_ini_key(&tree.join("absent.ini"), None, "anything"),
            None
        );
    }

    /// Pixels handed over rather than read off the disk: padded rows are
    /// stepped over, three channels means opaque, and a header that does not
    /// describe the buffer behind it is refused.
    ///
    /// The padding is the part worth a test. A sender is free to pad each row
    /// out to a convenient boundary, and reading the block as though it were
    /// tight does not fail — it draws the picture sheared into a diagonal
    /// smear, one row further wrong than the last.
    #[test]
    fn pixels_handed_over_are_read_the_way_they_were_described() {
        // Two red pixels a row, two rows, with four bytes of padding after
        // each. Anything reading it tightly picks the padding up as colour.
        let red = [255u8, 0, 0, 255];
        let padding = [9u8; 4];
        let mut padded = Vec::new();
        for _ in 0..2 {
            padded.extend_from_slice(&red);
            padded.extend_from_slice(&red);
            padded.extend_from_slice(&padding);
        }

        let icon = Icon::from_pixels(2, 2, 12, 4, &padded, 2).expect("a described picture");
        assert_eq!(icon.size, 2);
        assert_eq!(icon.rgba.len(), 2 * 2 * 4);
        assert!(
            icon.rgba.chunks_exact(4).all(|pixel| pixel == red),
            "every pixel is the colour that was sent, not the padding: {:?}",
            icon.rgba
        );

        // Three channels is a picture with nothing transparent in it, and the
        // alpha it does not carry is supplied rather than left at zero — an
        // icon that came out fully transparent would draw as nothing at all.
        let opaque = Icon::from_pixels(1, 1, 3, 3, &[12, 34, 56], 1).expect("three channels");
        assert_eq!(opaque.rgba, vec![12, 34, 56, 255]);

        // A stride shorter than a row, and a buffer shorter than the picture
        // it claims: both describe a walk off the end of what was sent.
        assert!(Icon::from_pixels(2, 2, 4, 4, &padded, 2).is_none());
        assert!(Icon::from_pixels(2, 2, 8, 4, &red, 2).is_none());
        assert!(Icon::from_pixels(0, 2, 8, 4, &padded, 2).is_none());
        assert!(Icon::from_pixels(2, 2, 8, 2, &padded, 2).is_none());
    }

    /// A machine with no desktop on it still gets a theme, because hicolor is
    /// not a working default — it is where a program puts its own icon, and it
    /// has never carried the standard names an announcement asks for.
    #[test]
    fn a_session_with_no_desktop_picks_a_theme_that_is_installed() {
        let tree = TestTree::new("themes");
        let root = tree.join("icons");
        let theme = |name: &str, body: &str| {
            std::fs::create_dir_all(root.join(name)).unwrap();
            write(&root.join(name).join("index.theme"), body);
        };

        // Nothing installed but hicolor is nothing to choose.
        theme("hicolor", "[Icon Theme]\nDirectories=48x48/apps\n");
        assert_eq!(any_installed_theme(std::slice::from_ref(&root)), None);

        // A cursor theme is an icon theme by file layout and carries no icons,
        // so picking one would be the same as picking nothing.
        theme("Bibata", "[Icon Theme]\nName=Bibata\n");
        assert_eq!(any_installed_theme(std::slice::from_ref(&root)), None);

        // Anything real will do when there is only one.
        theme("Zafiro", "[Icon Theme]\nDirectories=48x48/apps\n");
        assert_eq!(
            any_installed_theme(std::slice::from_ref(&root)).as_deref(),
            Some("Zafiro")
        );

        // And with a choice, the one most likely to be complete wins over the
        // one that happens to sort first.
        theme("Adwaita", "[Icon Theme]\nDirectories=48x48/apps\n");
        assert_eq!(
            any_installed_theme(std::slice::from_ref(&root)).as_deref(),
            Some("Adwaita")
        );
    }

    #[test]
    fn ranks_scalable_above_fixed_sizes() {
        assert!(directory_rank(Path::new("/t/scalable")) > directory_rank(Path::new("/t/512x512")));
        assert!(directory_rank(Path::new("/t/48x48")) > directory_rank(Path::new("/t/16x16")));
        assert_eq!(directory_rank(Path::new("/t/apps/128")), 128);
        assert_eq!(directory_rank(Path::new("/t/nonsense")), 0);
    }

    #[test]
    fn theme_chain_does_not_loop() {
        let tree = TestTree::new("inheritance-loop");
        write(
            &tree.join("a/index.theme"),
            "[Icon Theme]\nName=A\nInherits=b\n",
        );
        write(
            &tree.join("b/index.theme"),
            "[Icon Theme]\nName=B\nInherits=a\n",
        );

        let mut out = Vec::new();
        let mut seen = HashSet::new();
        collect_theme_chain(std::slice::from_ref(&tree.0), "a", &mut out, &mut seen);
        assert_eq!(out, vec!["a", "b"]);
    }

    #[test]
    fn dotted_desktop_icon_ids_are_not_mistaken_for_extensions() {
        let tree = TestTree::new("dotted-id");
        let directory = tree.join("icons");
        let icon_path = directory.join("org.example.Product.png");
        write(&icon_path, png_bytes(4, 4, [20, 40, 60, 255]));
        let mut loader = loader_with_dirs(vec![directory]);

        assert!(loader.load("org.example.Product", 8).is_some());
        assert_eq!(
            loader.cache.get("org.example.Product"),
            Some(&Some(icon_path))
        );
    }

    #[test]
    fn recognised_suffix_is_removed_without_truncating_other_dots() {
        let tree = TestTree::new("explicit-extension");
        let directory = tree.join("icons");
        let icon_path = directory.join("org.example.Product.png");
        write(&icon_path, png_bytes(4, 4, [20, 40, 60, 255]));
        let mut loader = loader_with_dirs(vec![directory]);

        assert!(loader.load("org.example.Product.png", 8).is_some());
        assert_eq!(
            loader.cache.get("org.example.Product.png"),
            Some(&Some(icon_path))
        );
    }

    #[test]
    fn svg_icons_can_embed_raster_images() {
        let tree = TestTree::new("svg-raster-image");
        let png = tree.join("tile.png");
        let svg = tree.join("icon.svg");
        write(&png, png_bytes(2, 2, [220, 40, 20, 255]));
        write(
            &svg,
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2">
                <image href="tile.png" width="2" height="2"/>
            </svg>"#,
        );

        let icon = load_path(&svg, 8).expect("embedded PNG should render");
        assert!(icon
            .rgba
            .chunks_exact(4)
            .any(|pixel| pixel[0] > 150 && pixel[3] > 200));
    }

    #[test]
    fn failed_high_priority_decode_falls_back_and_updates_cache() {
        let tree = TestTree::new("decode-fallback");
        let high = tree.join("high");
        let low = tree.join("low");
        let broken = high.join("sample.svg");
        let fallback = low.join("sample.png");
        write(&broken, "not an SVG");
        write(&fallback, png_bytes(2, 2, [12, 34, 56, 255]));
        let mut loader = loader_with_dirs(vec![high, low]);

        // A stale resolution cache may point at a now-broken file. Loading
        // must still continue into inherited themes when decoding it fails.
        loader.cache.insert("sample".to_string(), Some(broken));
        let icon = loader.load("sample", 2).expect("fallback PNG should load");
        assert_eq!(&icon.rgba[..4], &[12, 34, 56, 255]);
        assert_eq!(loader.cache.get("sample"), Some(&Some(fallback)));
    }

    #[test]
    fn recursively_discovers_nonstandard_theme_layouts() {
        let tree = TestTree::new("nested-layout");
        write(
            &tree.join("Odd/index.theme"),
            "[Icon Theme]\nName=Odd\nDirectories=apps/vector/scalable\n",
        );
        let icon_path = tree.join("Odd/apps/vector/scalable/deep-icon.png");
        write(&icon_path, png_bytes(3, 3, [80, 90, 100, 255]));

        let mut loader =
            IconLoader::from_paths(vec![tree.0.clone()], "Odd".to_string(), Vec::new());
        assert!(loader.load("deep-icon", 8).is_some());
        assert_eq!(loader.cache.get("deep-icon"), Some(&Some(icon_path)));
    }

    #[test]
    fn extensionless_absolute_png_and_svg_are_sniffed() {
        let tree = TestTree::new("extensionless");
        let png = tree.join("RasterIcon");
        let svg = tree.join("VectorIcon");
        write(&png, png_bytes(6, 4, [100, 110, 120, 255]));
        write(
            &svg,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="6">
                 <rect width="4" height="6" fill="#ff0000"/>
               </svg>"##,
        );

        for path in [png, svg] {
            let icon = load_path(&path, 8).expect("content sniffing should identify the icon");
            assert_eq!(icon.size, 8);
            assert_eq!(icon.rgba.len(), 8 * 8 * 4);
            assert!(icon.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0));
        }
    }

    #[test]
    fn non_square_rasters_are_centered_without_distortion() {
        let image = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            4,
            2,
            image::Rgba([10, 20, 30, 255]),
        ));
        let rgba = fit_raster(image, 8);

        let alpha = |x: usize, y: usize| rgba[(y * 8 + x) * 4 + 3];
        assert_eq!(alpha(4, 0), 0);
        assert_eq!(alpha(4, 2), 255);
        assert_eq!(alpha(4, 5), 255);
        assert_eq!(alpha(4, 7), 0);
    }

    #[test]
    fn svg_pixels_are_converted_back_to_straight_alpha() {
        let mut rgba = [50, 25, 10, 100, 9, 8, 7, 0, 4, 5, 6, 255];
        unpremultiply_rgba(&mut rgba);
        assert_eq!(&rgba[..4], &[128, 64, 26, 100]);
        assert_eq!(&rgba[4..8], &[9, 8, 7, 0]);
        assert_eq!(&rgba[8..], &[4, 5, 6, 255]);
    }

    /// The shell's own glyphs have to be in the binary and have to draw
    /// something, because nothing at runtime will notice if they do not: a
    /// glyph that fails to rasterise leaves an empty space in the sidebar,
    /// which looks like a layout that meant to leave one.
    ///
    /// The whole reason they are built in rather than looked up is that the
    /// quick-settings bars are for a machine with no desktop on it, and so
    /// possibly no icon theme beyond hicolor either.
    #[test]
    fn every_built_in_glyph_ships_and_draws_something() {
        assert_eq!(
            BUILTIN.len(),
            66,
            "a speaker, a struck-out one, a sun, a stick pointer, a mixer, a \
             moon, a \
             bell, two \
             controller buttons, four arrows, a keyboard folding away, a power \
             symbol, one per column of the category row, the two subcategories \
             Multimedia is divided into and the one under Graphics, the two \
             folders that stand for System's Files with the folder, page, drum \
             and house its own rows are drawn with, the \
             sixteen marks the Settings column is drawn from plus its four \
             turns of a monitor, the context menu's bin, play mark, ellipsis, sort bars \
             and camera, the magnifier at the head of a shelf with the \
             struck-through one that empties it, the padlock on the panel \
             that asks for a password, and the Steam column with the mark every \
             row that came out of it wears, the cycle that asks for the library \
             again and the door its account is left by"
        );

        for (name, drawing) in BUILTIN {
            assert!(name.starts_with("lxb:"), "{name} could be shadowed");
            assert!(drawing.contains("<svg"), "{name} is not a drawing");

            let icon = Icon::builtin(drawing, 128).unwrap_or_else(|| {
                panic!("{name} did not rasterise");
            });
            assert_eq!(icon.size, 128);
            assert_eq!(icon.rgba.len(), 128 * 128 * 4);

            // Ink, not an empty square. A drawing that misses its viewBox
            // rasterises perfectly happily to nothing at all.
            let ink = icon.rgba.chunks_exact(4).filter(|px| px[3] > 128).count();
            let share = ink as f32 / (128.0 * 128.0);
            assert!(
                (0.05..0.60).contains(&share),
                "{name} covers {share:.3} of its cell"
            );
            // Neutral, so that the colour the layout asks for is the colour it
            // gets: the atlas multiplies the quad's colour into the texel, and
            // a glyph with a hue of its own could only ever come out muddier
            // than the label beside it.
            //
            // Not the same as *white*. Everything the guide draws is shaded —
            // one lamp above the drawing, a shadow under it — and a grey under
            // a white tint is still that grey. What must not vary is the
            // balance between the channels.
            for pixel in icon.rgba.chunks_exact(4).filter(|px| px[3] == 255) {
                let [r, g, b] = [pixel[0], pixel[1], pixel[2]];
                assert!(
                    r.abs_diff(g) <= 1 && g.abs_diff(b) <= 1 && r.abs_diff(b) <= 1,
                    "{name} has a colour of its own: {:?}",
                    &pixel[..3]
                );
                // And never so dark that it reads as a hole in the glyph.
                assert!(r >= 128, "{name} has a pixel at {r}, which is not ink");
            }
        }

        // Distinct drawings, every one of them: muting has to be visible as
        // more than a change of alpha, a hint that named the same button twice
        // would be worse than no hint, and four arrow caps that rasterised
        // alike would point the wrong way three times out of four.
        let names: Vec<&str> = BUILTIN.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            vec![
                VOLUME,
                VOLUME_MUTED,
                BRIGHTNESS,
                POINTER_STICK,
                VOLUME_MIXER,
                DO_NOT_DISTURB,
                NOTIFICATIONS,
                PAD_SELECT,
                PAD_WEST,
                ARROW_LEFT,
                ARROW_DOWN,
                ARROW_UP,
                ARROW_RIGHT,
                KEYBOARD_HIDE,
                SHUTDOWN,
                CATEGORY_SETTINGS,
                CATEGORY_SYSTEM,
                CATEGORY_MULTIMEDIA,
                CATEGORY_GRAPHICS,
                CATEGORY_INTERNET,
                CATEGORY_OFFICE,
                CATEGORY_GAMES,
                CATEGORY_DEVELOPMENT,
                CATEGORY_EDUCATION,
                CATEGORY_UTILITIES,
                CATEGORY_OTHER,
                CATEGORY_MUSIC,
                CATEGORY_VIDEO,
                CATEGORY_IMAGES,
                CATEGORY_FILES,
                FILE_FOLDER,
                FILE_PAGE,
                FILE_DRIVE,
                FILE_HOME,
                SETTING_APPEARANCE,
                SETTING_ACCENT,
                SETTING_DISPLAY,
                SETTING_RESOLUTION,
                SETTING_REFRESH,
                SETTING_ORIENTATION,
                SETTING_ROTATION_0,
                SETTING_ROTATION_90,
                SETTING_ROTATION_180,
                SETTING_ROTATION_270,
                SETTING_ORDER,
                SETTING_NIGHT_LIGHT,
                SETTING_SCHEDULE,
                SETTING_HDR,
                SETTING_MICROPHONE,
                SETTING_SYSTEM,
                SETTING_SCALE,
                SETTING_INFO,
                SWATCH,
                CHOSEN,
                UNINSTALL,
                LAUNCH,
                OPEN_WITH,
                SORT,
                SCREENSHOT,
                SEARCH,
                SEARCH_CLEAR,
                AUTHENTICATE,
                CATEGORY_STEAM,
                STEAM,
                REFRESH,
                SIGN_OUT
            ]
        );
        let drawn: Vec<Vec<u8>> = BUILTIN
            .iter()
            .map(|(_, drawing)| Icon::builtin(drawing, 64).unwrap().rgba)
            .collect();
        for (first, left) in drawn.iter().enumerate() {
            for (second, right) in drawn.iter().enumerate().skip(first + 1) {
                assert_ne!(
                    left, right,
                    "{} and {} rasterise the same",
                    names[first], names[second]
                );
            }
        }
    }
}
