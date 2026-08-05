//! Icon theme lookup and decoding.
//!
//! Implements the parts of the freedesktop icon theme specification that a
//! launcher actually needs: search the current theme, follow `Inherits` from
//! `index.theme`, fall back to hicolor and then `/usr/share/pixmaps`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const ICON_EXTENSIONS: [&str; 3] = ["svg", "png", "xpm"];
const MAX_THEME_DEPTH: usize = 8;

/// A decoded icon, always square RGBA8.
pub struct Icon {
    pub size: u32,
    pub rgba: Vec<u8>,
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

fn rasterise_svg(data: &[u8], resources_dir: Option<&Path>, size: u32) -> Option<Vec<u8>> {
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
    if let Ok(theme) = std::env::var("LINBOARD_ICON_THEME") {
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
                "linboard-icon-test-{label}-{}-{}",
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
}
