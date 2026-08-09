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
/// The two tiles above the quick-settings bars: driving the pointer from the
/// right stick, and the per-application volume mixer.
///
/// Every glyph in the guide is lit by the same lamp above it, but these two
/// carry the most of that modelling: a gloss boundary that curves with the
/// object, shadow cast from one part onto the next, and controls sunk into
/// wells. They can afford it because they are the largest of the set — a tile
/// gives its glyph 42 px where a quick-settings bar gives 33 and the keyboard
/// hint 27, and a seam or a well drawn at 27 px is three grey pixels of
/// smudge. How much of the modelling each glyph keeps is a question of the
/// size it is drawn at and not of style; see brightness.svg for the reduction.
pub const POINTER_STICK: &str = "lxb:pointer-stick";
pub const VOLUME_MIXER: &str = "lxb:volume-mixer";
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

/// The guide menu's Screenshot row.
///
/// Built in like the two above, and for the third reason again: `camera-photo`
/// is a coloured icon from whichever theme is installed, and the row it sits in
/// has one lamp over it and two arrows drawn under that lamp already.
pub const SCREENSHOT: &str = "lxb:screenshot";

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
pub const BUILTIN: [(&str, &str); 46] = [
    (VOLUME, include_str!("glyphs/volume.svg")),
    (VOLUME_MUTED, include_str!("glyphs/volume-muted.svg")),
    (BRIGHTNESS, include_str!("glyphs/brightness.svg")),
    (POINTER_STICK, include_str!("glyphs/pointer-stick.svg")),
    (VOLUME_MIXER, include_str!("glyphs/volume-mixer.svg")),
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
    (SETTING_HDR, include_str!("glyphs/setting-hdr.svg")),
    (SETTING_INFO, include_str!("glyphs/setting-info.svg")),
    (SWATCH, include_str!("glyphs/swatch.svg")),
    (CHOSEN, include_str!("glyphs/chosen.svg")),
    // The context menu's own rows.
    (UNINSTALL, include_str!("glyphs/uninstall.svg")),
    (LAUNCH, include_str!("glyphs/launch.svg")),
    (OPEN_WITH, include_str!("glyphs/open-with.svg")),
    (SORT, include_str!("glyphs/sort.svg")),
    (SCREENSHOT, include_str!("glyphs/screenshot.svg")),
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
    if let Some(theme) = read_kde_theme() {
        return theme;
    }
    "hicolor".to_string()
}

fn read_kde_theme() -> Option<String> {
    let home = std::env::var_os("HOME")?;
    let path = PathBuf::from(home).join(".config/kdeglobals");
    let raw = std::fs::read_to_string(path).ok()?;

    let mut in_icons = false;
    for line in raw.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_icons = line == "[Icons]";
            continue;
        }
        if in_icons {
            if let Some(value) = line.strip_prefix("Theme=") {
                let value = value.trim();
                if !value.is_empty() {
                    return Some(value.to_string());
                }
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
            46,
            "a speaker, a struck-out one, a sun, a stick pointer, a mixer, two \
             controller buttons, four arrows, a keyboard folding away, a power \
             symbol, one per column of the category row, the two subcategories \
             Multimedia is divided into and the one under Graphics, the ten \
             marks the Settings column is drawn from plus its four turns of a \
             monitor, and the context menu's bin, play mark, ellipsis, sort \
             bars and camera"
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
                SETTING_HDR,
                SETTING_INFO,
                SWATCH,
                CHOSEN,
                UNINSTALL,
                LAUNCH,
                OPEN_WITH,
                SORT,
                SCREENSHOT
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
