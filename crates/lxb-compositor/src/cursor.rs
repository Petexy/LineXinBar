//! The compositor-drawn cursor.
//!
//! Clients that supply a cursor surface get that surface rendered as-is. For
//! everything else — the desktop itself, and clients that only *name* a shape
//! through `cursor-shape-v1` — the shape is looked up in an XCursor theme.
//!
//! The theme comes from `XCURSOR_THEME` / `XCURSOR_SIZE`, which `main`
//! exports from the config before any backend starts. LineXinBar bundles the
//! Bibata Modern Classic theme under `share/icons/`, and the default arrow
//! from it is additionally compiled into the binary, so a pointer exists even
//! on a system with no cursor theme installed at all.

use std::collections::HashMap;
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

/// The bundled Bibata arrow, so [`CursorState`] can always produce *some*
/// pointer: a missing theme must degrade to a plainer cursor, never to an
/// invisible one.
const EMBEDDED_ARROW: &[u8] =
    include_bytes!("../../../share/icons/Bibata-Modern-Classic/cursors/left_ptr");

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
        let theme_name =
            std::env::var("XCURSOR_THEME").unwrap_or_else(|_| "Bibata-Modern-Classic".into());
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

/// Export `XCURSOR_THEME` / `XCURSOR_SIZE` / `XCURSOR_PATH` for this process
/// and everything it starts.
///
/// One exported truth keeps three consumers agreeing on the pointer: the
/// compositor's own [`CursorState`], client toolkits drawing their cursors,
/// and XWayland. The bundled theme directory is prepended to `XCURSOR_PATH`
/// so the cursors work from a build tree or an installed prefix alike,
/// without touching `/usr/share/icons`.
pub fn export_cursor_environment(theme: Option<&str>, size: Option<u32>) {
    let theme = theme
        .map(str::to_string)
        .or_else(|| std::env::var("XCURSOR_THEME").ok())
        .unwrap_or_else(|| "Bibata-Modern-Classic".to_string());
    let size = size
        .or_else(|| {
            std::env::var("XCURSOR_SIZE")
                .ok()
                .and_then(|raw| raw.parse().ok())
        })
        .filter(|size| *size > 0)
        .unwrap_or(DEFAULT_SIZE);

    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("LXB_DATA_DIR") {
        paths.push(std::path::PathBuf::from(dir).join("icons"));
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
                let home = std::path::PathBuf::from(home);
                paths.push(home.join(".icons"));
                paths.push(home.join(".local/share/icons"));
            }
            paths.push("/usr/share/icons".into());
            paths.push("/usr/local/share/icons".into());
            paths.push("/usr/share/pixmaps".into());
        }
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
}
