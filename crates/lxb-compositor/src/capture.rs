//! Photographing one window.
//!
//! The shell can draw anything it likes over an application, and it cannot see
//! a single pixel of it: a Wayland client reads its own surfaces and nothing
//! else, which is the point of Wayland. So a screenshot of the window a user is
//! pointing at has to be taken here, where the buffers actually are.
//!
//! What comes out is the *window*, not the screen it is on: its own contents at
//! its own size, with nothing in front of it, nothing behind it, and none of
//! the shell's own glass over it. That is what the guide's menu offers — the
//! entry says "the app" — and it is also the only version of the picture that
//! is worth having, because the display it was on is mostly this window anyway.
//!
//! Rendering is done with the same element list the display uses, into an
//! offscreen texture instead of a screen, and read back through [`ExportMem`].
//! Reusing the elements is what keeps the picture honest: a client drawing
//! subsurfaces is drawn here exactly as it is drawn there, rather than through
//! a second path that would quietly forget about them.

use std::io::BufWriter;
use std::path::Path;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::AsRenderElements;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::{ExportMem, ImportAll, ImportMem, Offscreen, Renderer};
use smithay::desktop::Window;
use smithay::utils::{Buffer as BufferCoords, Point, Rectangle, Scale, Size, Transform};

/// A captured window: its pixels, and how they are laid out.
///
/// Bytes are R, G, B, A in that order, top row first, which is what a PNG
/// wants and what the renderer is asked for.
pub struct Shot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Render `window` on its own into an offscreen buffer and read it back.
///
/// `scale` is its display's, so the picture has as many pixels as the screen is
/// actually showing rather than as many as the window's logical size suggests.
///
/// Generic over the renderer for the reason [`crate::render`] is: the nested
/// backends hold a `GlesRenderer` and the DRM one a multi-GPU renderer over the
/// same, and a screenshot that only worked on one of them would be a screenshot
/// that only worked in the debugger.
pub fn window<R>(renderer: &mut R, window: &Window, scale: f64) -> anyhow::Result<Shot>
where
    R: Renderer + ImportAll + ImportMem + ExportMem + Offscreen<GlesTexture>,
    R::TextureId: Send + Clone + 'static,
    R::Error: Send + Sync + 'static,
{
    // The window's *geometry* — the frame the user thinks of as the window —
    // rather than its whole surface. A client drawing its own decorations hangs
    // a drop shadow off every side, and a screenshot with a band of the
    // application's own shadow around it is a screenshot nobody asked for.
    let geometry = window.geometry();
    let size = geometry.size.to_physical_precise_round(scale);
    if size.w <= 0 || size.h <= 0 {
        anyhow::bail!("the window has no size to photograph");
    }
    // The same rectangle again in buffer coordinates, which is what a texture
    // is measured in. One picture, two ways of saying how big it is: the
    // renderer draws in physical pixels and the buffer holds them.
    let buffer: Size<i32, BufferCoords> = Size::from((size.w, size.h));

    let mut texture = renderer
        .create_buffer(Fourcc::Abgr8888, buffer)
        .map_err(|err| anyhow::anyhow!("no offscreen buffer to draw into: {err}"))?;
    let mut framebuffer = renderer
        .bind(&mut texture)
        .map_err(|err| anyhow::anyhow!("could not draw into the offscreen buffer: {err}"))?;

    // Placed so that the corner of the geometry lands on the corner of the
    // picture: the surface starts a shadow's width before it, and that width is
    // exactly what falls off the edge of the buffer.
    let location = Point::from((-geometry.loc.x, -geometry.loc.y)).to_physical_precise_round(scale);
    let elements = window.render_elements::<WaylandSurfaceRenderElement<R>>(
        renderer,
        location,
        Scale::from(scale),
        1.0,
    );

    // Transparent, not black. A window with rounded corners of its own has
    // nothing behind it here, and inventing a colour for that would be
    // inventing part of the picture.
    OutputDamageTracker::new(size, scale, Transform::Normal)
        .render_output(renderer, &mut framebuffer, 0, &elements, [0.0; 4])
        .map_err(|err| anyhow::anyhow!("could not render the window: {err:?}"))?;

    let mapping = renderer
        .copy_framebuffer(&framebuffer, Rectangle::from_size(buffer), Fourcc::Abgr8888)
        .map_err(|err| anyhow::anyhow!("could not read the window back: {err}"))?;
    let bytes = renderer
        .map_texture(&mapping)
        .map_err(|err| anyhow::anyhow!("could not map the captured pixels: {err}"))?;

    Ok(Shot {
        width: size.w as u32,
        height: size.h as u32,
        rgba: bytes.to_vec(),
    })
}

/// Write a shot out as a PNG.
///
/// The directory is expected to exist: whoever chose the path chose where the
/// user's pictures go, and a compositor quietly creating directories somebody
/// named at it is a compositor with an opinion about the user's home.
pub fn write_png(shot: &Shot, path: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), shot.width, shot.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(&shot.rgba)?;
    writer.finish()?;
    Ok(())
}
