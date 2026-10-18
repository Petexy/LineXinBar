//! The compositor-drawn cursor.
//!
//! Clients that supply a cursor surface get that surface rendered as-is. For
//! everything else — the desktop itself, and clients that only *name* a shape
//! through `cursor-shape-v1` — the shape is looked up in an XCursor theme.
//!
//! The theme comes from `XCURSOR_THEME` / `XCURSOR_SIZE`, which `main`
//! exports from the config before any backend starts. LineXinBar bundles the
//! Bibata Modern Classic theme under `share/icons/`, in a directory of its own
//! ([`BUNDLED_THEME`]), and the default arrow from it is additionally compiled
//! into the binary, so a pointer exists even on a system with no cursor theme
//! installed at all.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::surface::render_elements_from_surface_tree;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::{ImportAll, ImportMem, Renderer};
use smithay::input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData};
use smithay::utils::{Physical, Point, Scale, Transform};
use smithay::wayland::compositor;

use crate::render::LxbRenderElement;

/// The bundled theme: its directory under `share/icons/`, and so the name
/// `XCURSOR_THEME` gives it.
///
/// It is Bibata Modern Classic, which a person may not have, so LineXinBar
/// carries its own copy — under a name of its own. linexin-desktop ships the
/// same theme as `/usr/share/icons/Bibata-Modern-Classic/`, and two packages
/// that own one file cannot be installed together: pacman refuses the whole
/// transaction, which is what stopped a medium carrying both desktops from
/// being built. The `lxb-` prefix is the family's mark for a third party's
/// work it carries a copy of.
pub const BUNDLED_THEME: &str = "lxb-Bibata-Modern-Classic";

/// What [`BUNDLED_THEME`] was called until 0.9.4, and so what a setting written
/// before then names. See [`resolve_theme`].
const FORMER_BUNDLED_THEME: &str = "Bibata-Modern-Classic";

/// The bundled Bibata arrow, so [`CursorState`] can always produce *some*
/// pointer: a missing theme must degrade to a plainer cursor, never to an
/// invisible one. The path is [`BUNDLED_THEME`]'s, written out because
/// `include_bytes!` takes nothing but a literal.
const EMBEDDED_ARROW: &[u8] =
    include_bytes!("../../../share/icons/lxb-Bibata-Modern-Classic/cursors/left_ptr");

/// Nominal size when neither the config nor `XCURSOR_SIZE` says otherwise.
/// 24 is what every desktop environment defaults to.
const DEFAULT_SIZE: u32 = 24;

/// One frame of a (possibly animated) cursor, uploaded lazily per renderer.
struct Frame {
    buffer: MemoryRenderBuffer,
    /// In buffer pixels. The buffer's scale matches the output's, so this is
    /// also the physical offset from the pointer tip to the image origin.
    hotspot: Point<i32, Physical>,
    delay_ms: u32,
}

/// A decoded cursor at one scale: every animation frame plus the cycle length.
struct Cursor {
    frames: Vec<Frame>,
    total_delay_ms: u32,
}

impl Cursor {
    /// Keep the frames whose nominal size is nearest to `nominal * scale`.
    ///
    /// XCursor files carry several sizes interleaved; the frames of one
    /// animation share a nominal size and appear in playback order.
    fn build(images: &[xcursor::parser::Image], nominal: u32, scale: i32) -> Option<Self> {
        let desired = nominal.saturating_mul(scale.max(1) as u32);
        let best = images
            .iter()
            .map(|image| image.size)
            .min_by_key(|size| size.abs_diff(desired))?;

        let frames: Vec<Frame> = images
            .iter()
            .filter(|image| image.size == best)
            .map(|image| Frame {
                buffer: MemoryRenderBuffer::from_slice(
                    &image.pixels_rgba,
                    Fourcc::Abgr8888,
                    (image.width as i32, image.height as i32),
                    scale,
                    Transform::Normal,
                    None,
                ),
                hotspot: Point::from((image.xhot as i32, image.yhot as i32)),
                delay_ms: image.delay,
            })
            .collect();

        let total_delay_ms = images
            .iter()
            .filter(|image| image.size == best)
            .map(|image| image.delay)
            .sum();

        (!frames.is_empty()).then_some(Self {
            frames,
            total_delay_ms,
        })
    }

    /// The frame to show at `time`, looping over the animation. A static
    /// cursor (one frame, or all delays zero) always yields its first frame.
    fn frame_at(&self, time: Duration) -> Option<&Frame> {
        if self.frames.len() <= 1 || self.total_delay_ms == 0 {
            return self.frames.first();
        }
        let mut remainder = (time.as_millis() % self.total_delay_ms as u128) as u32;
        for frame in &self.frames {
            if remainder < frame.delay_ms {
                return Some(frame);
            }
            remainder -= frame.delay_ms;
        }
        self.frames.first()
    }
}

pub struct CursorState {
    theme: xcursor::CursorTheme,
    size: u32,
    /// Decoded cursors by shape name and integer scale. Misses are cached too,
    /// so an absent shape costs one theme walk rather than one per frame.
    cache: HashMap<(&'static str, i32), Option<Rc<Cursor>>>,
    pub status: CursorImageStatus,
}

impl CursorState {
    pub fn new() -> Self {
        // Already resolved: `main` exports the theme, through
        // [`resolve_theme`], before any backend makes one of these.
        let theme_name = std::env::var("XCURSOR_THEME").unwrap_or_else(|_| BUNDLED_THEME.into());
        let size = std::env::var("XCURSOR_SIZE")
            .ok()
            .and_then(|raw| raw.parse().ok())
            .filter(|size| *size > 0)
            .unwrap_or(DEFAULT_SIZE);
        tracing::info!(theme = %theme_name, size, "loading cursor theme");

        Self {
            theme: xcursor::CursorTheme::load(&theme_name),
            size,
            cache: HashMap::new(),
            status: CursorImageStatus::default_named(),
        }
    }

    /// Draw at a different nominal size from now on.
    ///
    /// The cache is keyed by shape and integer scale rather than by size —
    /// there is one size at a time — so it has to go with the number: what is
    /// in it are pictures decoded for the size that was in force. Left alone,
    /// the pointer would go on being drawn at the old size until it happened to
    /// change shape.
    ///
    /// A zero is ignored rather than obeyed. It is what an empty or unparsable
    /// `XCURSOR_SIZE` looks like arriving from somewhere else, and a cursor
    /// nought pixels across is a session with no pointer at all.
    pub fn set_size(&mut self, size: u32) {
        if size == 0 || size == self.size {
            return;
        }
        tracing::info!(size, "the cursor's size");
        self.size = size;
        self.cache.clear();
    }

    /// Render the cursor at `position` (the pointer tip, in physical pixels).
    pub fn render<R>(
        &mut self,
        renderer: &mut R,
        position: Point<i32, Physical>,
        scale: Scale<f64>,
        time: Duration,
    ) -> Vec<LxbRenderElement<R>>
    where
        R: Renderer + ImportAll + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        match &self.status {
            // A hidden cursor draws nothing at all.
            CursorImageStatus::Hidden => Vec::new(),
            CursorImageStatus::Surface(surface) => {
                // The surface's origin is not the pointer tip; the client says
                // where the tip lies inside the image.
                let hotspot = compositor::with_states(surface, |states| {
                    states
                        .data_map
                        .get::<CursorImageSurfaceData>()
                        .map(|data| data.lock().unwrap().hotspot)
                        .unwrap_or_default()
                })
                .to_physical_precise_round(scale);

                render_elements_from_surface_tree(
                    renderer,
                    surface,
                    position - hotspot,
                    scale,
                    1.0,
                    Kind::Cursor,
                )
                .into_iter()
                .map(LxbRenderElement::Surface)
                .collect()
            }
            CursorImageStatus::Named(icon) => {
                let icon = *icon;
                // The buffer scale is integral; fractional outputs get the
                // next size up and scale it down, which keeps the edge crisp.
                let scale = (scale.x.max(scale.y).ceil() as i32).max(1);
                let Some(cursor) = self.themed(icon, scale) else {
                    return Vec::new();
                };
                let Some(frame) = cursor.frame_at(time) else {
                    return Vec::new();
                };

                match MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    (position - frame.hotspot).to_f64(),
                    &frame.buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                ) {
                    Ok(element) => vec![LxbRenderElement::Memory(element)],
                    Err(err) => {
                        tracing::warn!(?err, "could not upload the cursor image");
                        Vec::new()
                    }
                }
            }
        }
    }

    fn themed(&mut self, icon: CursorIcon, scale: i32) -> Option<Rc<Cursor>> {
        let key = (icon.name(), scale);
        if let Some(cached) = self.cache.get(&key) {
            return cached.clone();
        }

        let loaded = self.decode(icon, scale);
        self.cache.insert(key, loaded.clone());
        loaded
    }

    fn decode(&mut self, icon: CursorIcon, scale: i32) -> Option<Rc<Cursor>> {
        // The theme may know the shape under its CSS name or an X11 alias.
        let names = std::iter::once(icon.name()).chain(icon.alt_names().iter().copied());
        for name in names {
            let Some(path) = self.theme.load_icon(name) else {
                continue;
            };
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if let Some(cursor) = xcursor::parser::parse_xcursor(&bytes)
                .and_then(|images| Cursor::build(&images, self.size, scale))
            {
                return Some(Rc::new(cursor));
            }
        }

        // A shape the theme lacks falls back to the arrow: a wrong-but-visible
        // pointer beats a vanished one.
        if icon != CursorIcon::Default {
            tracing::debug!(
                shape = icon.name(),
                "cursor shape not in theme, using arrow"
            );
            return self.themed(CursorIcon::Default, scale);
        }

        xcursor::parser::parse_xcursor(EMBEDDED_ARROW)
            .and_then(|images| Cursor::build(&images, self.size, scale))
            .map(Rc::new)
    }
}

impl Default for CursorState {
    fn default() -> Self {
        Self::new()
    }
}

/// The theme to load for one asked for by name.
///
/// Every name is taken at its word but one: the name the bundled theme went by
/// until 0.9.4. A setting written before then still names it — `cursor_theme`
/// in the configuration, the example configuration's line, an `XCURSOR_THEME`
/// handed down by the session — and since the copy moved to a directory of its
/// own, nothing answers to that name unless somebody else's Bibata is
/// installed. linexin-desktop's is, on a machine with Linexin's GNOME desktop;
/// then that is the theme the setting names, and it is the one used. Otherwise
/// the name means what it meant when it was written, the copy LineXinBar
/// carries, so nobody's pointer changes or disappears across the upgrade.
///
/// `installed` says whether a theme of a name is anywhere a theme is looked
/// for; [`theme_installed`] is the answer the session uses.
fn resolve_theme(requested: &str, installed: impl Fn(&str) -> bool) -> &str {
    if requested == FORMER_BUNDLED_THEME && !installed(requested) {
        BUNDLED_THEME
    } else {
        requested
    }
}

/// Whether a cursor theme of this name is in one of `search_paths`: a directory
/// of that name with cursors of its own, which is what an XCursor theme is.
fn theme_installed(name: &str, search_paths: &[PathBuf]) -> bool {
    search_paths
        .iter()
        .any(|dir| dir.join(name).join("cursors").is_dir())
}

/// Export `XCURSOR_THEME` / `XCURSOR_SIZE` / `XCURSOR_PATH` for this process
/// and everything it starts.
///
/// One exported truth keeps three consumers agreeing on the pointer: the
/// compositor's own [`CursorState`], client toolkits drawing their cursors,
/// and XWayland. The bundled theme directory is prepended to `XCURSOR_PATH`
/// so the cursors work from a build tree or an installed prefix alike,
/// without touching `/usr/share/icons`. The theme is exported resolved (see
/// [`resolve_theme`]), so every one of the three is told a name that exists.
pub fn export_cursor_environment(theme: Option<&str>, size: Option<u32>) {
    let requested = theme
        .map(str::to_string)
        .or_else(|| std::env::var("XCURSOR_THEME").ok())
        .unwrap_or_else(|| BUNDLED_THEME.to_string());
    let size = size
        .or_else(|| {
            std::env::var("XCURSOR_SIZE")
                .ok()
                .and_then(|raw| raw.parse().ok())
        })
        .filter(|size| *size > 0)
        .unwrap_or(DEFAULT_SIZE);

    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("LXB_DATA_DIR") {
        paths.push(PathBuf::from(dir).join("icons"));
    }
    if let Ok(exe) = std::env::current_exe() {
        // <prefix>/bin/lxb -> <prefix>/share/icons, and
        // target/<profile>/lxb -> <checkout>/share/icons.
        for up in [1, 2] {
            let mut dir = exe.clone();
            for _ in 0..=up {
                dir.pop();
            }
            let icons = dir.join("share/icons");
            if icons.is_dir() {
                paths.push(icons);
            }
        }
    }
    // Setting XCURSOR_PATH replaces the default search list, so put the
    // standard locations back after ours.
    match std::env::var("XCURSOR_PATH") {
        Ok(existing) => paths.extend(std::env::split_paths(&existing)),
        Err(_) => {
            if let Some(home) = std::env::var_os("HOME") {
                let home = PathBuf::from(home);
                paths.push(home.join(".icons"));
                paths.push(home.join(".local/share/icons"));
            }
            paths.push("/usr/share/icons".into());
            paths.push("/usr/local/share/icons".into());
            paths.push("/usr/share/pixmaps".into());
        }
    }

    let theme = resolve_theme(&requested, |name| theme_installed(name, &paths)).to_string();
    if theme != requested {
        tracing::info!(
            requested = %requested,
            theme = %theme,
            "the cursor theme setting names the bundled theme by its former name"
        );
    }

    if let Ok(joined) = std::env::join_paths(paths) {
        std::env::set_var("XCURSOR_PATH", joined);
    }
    std::env::set_var("XCURSOR_THEME", &theme);
    std::env::set_var("XCURSOR_SIZE", size.to_string());
    tracing::debug!(theme = %theme, size, "cursor environment exported");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn animated(delays: &[u32]) -> Cursor {
        let frames = delays
            .iter()
            .map(|&delay_ms| Frame {
                buffer: MemoryRenderBuffer::new(
                    Fourcc::Abgr8888,
                    (1, 1),
                    1,
                    Transform::Normal,
                    None,
                ),
                hotspot: Point::from((0, 0)),
                delay_ms,
            })
            .collect::<Vec<_>>();
        let total_delay_ms = delays.iter().sum();
        Cursor {
            frames,
            total_delay_ms,
        }
    }

    #[test]
    fn embedded_arrow_parses_and_carries_a_hotspot() {
        // The compiled-in fallback is the last line of defence; if it cannot
        // be decoded the pointer can vanish entirely.
        let images = xcursor::parser::parse_xcursor(EMBEDDED_ARROW)
            .expect("bundled cursor should be a valid XCursor file");
        for scale in [1, 2] {
            let cursor = Cursor::build(&images, DEFAULT_SIZE, scale)
                .expect("bundled cursor should offer a usable size");
            let frame = cursor.frame_at(Duration::ZERO).unwrap();
            assert!(frame.hotspot.x >= 0 && frame.hotspot.y >= 0);
        }
    }

    #[test]
    fn animation_frames_advance_and_loop() {
        let cursor = animated(&[10, 20, 30]);
        let frame_index = |ms: u64| {
            let frame = cursor.frame_at(Duration::from_millis(ms)).unwrap();
            cursor
                .frames
                .iter()
                .position(|f| std::ptr::eq(f, frame))
                .unwrap()
        };

        assert_eq!(frame_index(0), 0);
        assert_eq!(frame_index(15), 1);
        assert_eq!(frame_index(35), 2);
        // 60ms is one full cycle, so the animation starts over.
        assert_eq!(frame_index(60), 0);
        assert_eq!(frame_index(75), 1);
    }

    #[test]
    fn static_cursors_ignore_time() {
        let cursor = animated(&[0]);
        assert!(cursor.frame_at(Duration::from_secs(1000)).is_some());
    }

    /// The checkout's own `share/icons/`, which holds the bundled theme and
    /// nothing else — never a directory this machine's themes are in.
    fn tree_icons() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../share/icons")
    }

    #[test]
    fn the_former_name_is_the_bundled_copy_when_nothing_answers_to_it() {
        // A configuration written before 0.9.4, on a machine without
        // linexin-desktop: the pointer it always had.
        assert_eq!(
            resolve_theme("Bibata-Modern-Classic", |_| false),
            BUNDLED_THEME
        );
    }

    #[test]
    fn the_former_name_is_linexin_desktops_bibata_when_that_is_installed() {
        // The same setting beside Linexin's GNOME desktop names a theme that
        // exists, and the theme that exists is the one used.
        let installed = |name: &str| name == "Bibata-Modern-Classic";
        assert_eq!(
            resolve_theme("Bibata-Modern-Classic", installed),
            "Bibata-Modern-Classic"
        );
    }

    #[test]
    fn every_other_name_is_taken_at_its_word() {
        // Installed or not: a theme that is missing degrades to the arrow at
        // draw time, and only the former name has a meaning to fall back to.
        for name in [BUNDLED_THEME, "Adwaita", "bibata-modern-classic", ""] {
            assert_eq!(resolve_theme(name, |_| false), name);
            assert_eq!(resolve_theme(name, |_| true), name);
        }
    }

    #[test]
    fn the_bundled_theme_never_answers_to_the_name_linexin_desktop_ships() {
        // The whole point of the directory's name: the tree that is packaged
        // has the bundled theme, and nothing a second package installs too.
        let tree = [tree_icons()];
        assert!(theme_installed(BUNDLED_THEME, &tree));
        assert!(!theme_installed(FORMER_BUNDLED_THEME, &tree));
        assert!(!theme_installed(BUNDLED_THEME, &[]));
        // So in a session that can see only the tree, the former name is
        // carried to the copy that is there.
        assert_eq!(
            resolve_theme(FORMER_BUNDLED_THEME, |name| theme_installed(name, &tree)),
            BUNDLED_THEME
        );
    }

    #[test]
    fn the_compiled_arrow_is_the_bundled_themes_own() {
        // `include_bytes!` cannot take the constant, so the path is written
        // twice; this is what keeps the two the same directory.
        let arrow = tree_icons().join(BUNDLED_THEME).join("cursors/left_ptr");
        assert_eq!(std::fs::read(arrow).unwrap(), EMBEDDED_ARROW);
    }
}
