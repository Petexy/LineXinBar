//! Photographing one window, and photographing one display.
//!
//! The shell can draw anything it likes over an application, and it cannot see
//! a single pixel of it: a Wayland client reads its own surfaces and nothing
//! else, which is the point of Wayland. So a screenshot of the window a user is
//! pointing at has to be taken here, where the buffers actually are.
//!
//! There are two pictures, and they answer different questions.
//!
//! [`window`] is *the application*: its own contents at its own size, with
//! nothing in front of it, nothing behind it, and none of the shell's own glass
//! over it. That is what the guide's menu offers — the entry says "the app".
//!
//! [`output`] is *the screen*: the same composite the display is showing, in
//! the same order, wallpaper and windows and the shell's bar over them, at as
//! many pixels as that display is driven at. That is what a screenshot key
//! means everywhere else, and it is the only one of the two that can be taken
//! of a display with nothing running on it.
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
use smithay::backend::renderer::element::{AsRenderElements, RenderElement};
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::{ExportMem, ImportAll, ImportMem, Offscreen, Renderer};
use smithay::desktop::Window;
use smithay::output::Output;
use smithay::utils::{Buffer as BufferCoords, Physical, Point, Rectangle, Scale, Size, Transform};

use crate::state::Lxb;

/// A captured window or display: its pixels, and how they are laid out.
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
    shoot(
        renderer,
        size,
        &mut OutputDamageTracker::new(size, scale, Transform::Normal),
        &elements,
        [0.0; 4],
    )
}

/// Render everything on `output` into an offscreen buffer and read it back.
///
/// The composite, not a list of windows: the element list is [`crate::render`]'s
/// own, so what lands in the picture is what the display is drawing, in the
/// order it draws it — which is the whole difference between this and
/// [`window`].
///
/// The picture is the size the display *shows*, which for a screen standing on
/// its side is not the size the connector scans out: a panel driven at
/// 1920×1080 and turned a quarter turn shows a 1080×1920 picture, and that is
/// the one somebody looking at it sees. So the buffer is that way round and the
/// contents are drawn upright in it, rather than being the panel's own
/// landscape frame with everything lying on its side inside it — which is what
/// the display's own damage tracker would produce, because a scanout buffer is
/// exactly what that one is for. See [`crate::screencopy`], which wants the
/// same picture for the same reason.
///
/// The cursor is left out. It is not part of any surface — the compositor draws
/// it — and it is not on screen at all while the session is being driven from a
/// controller, so a picture with an arrow in it would be a picture of a moment
/// that never happened on most of the screenshots this session takes.
pub fn output<R>(renderer: &mut R, lxb: &Lxb, output: &Output) -> anyhow::Result<Shot>
where
    R: Renderer + ImportAll + ImportMem + ExportMem + Offscreen<GlesTexture>,
    R::TextureId: Send + Clone + 'static,
    R::Error: Send + Sync + 'static,
{
    let size = crate::screencopy::picture_size(output)
        .ok_or_else(|| anyhow::anyhow!("the display has no size to photograph"))?;

    let elements = crate::render::output_elements(renderer, lxb, output, None);

    // The session's own background behind it all, exactly as the backends clear
    // to: the gap a display with nothing on it shows is part of the picture.
    shoot(
        renderer,
        size,
        &mut OutputDamageTracker::new(
            size,
            output.current_scale().fractional_scale(),
            Transform::Normal,
        ),
        &elements,
        lxb.config.general.background,
    )
}

/// Which side of the shell's own surfaces a picture is of.
///
/// The shell draws its session on one surface, and the compositor draws
/// something on each side of it: the application windows behind, the windows a
/// picture-in-picture setting floats in front. A pane of glass on the shell's
/// surface refracts what is behind that surface; one on the surface its context
/// menu is drawn on refracts both, with the shell's own frame between them. So
/// they are two pictures, not one — see `lxb_shell_v1.ask_for_the_picture_behind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The application windows: between the shell's wallpaper and the surface
    /// it draws its session on.
    Below,
    /// The floating windows, and anything flying back out of a tile in front of
    /// the shell: drawn over that surface and under its context menu.
    Above,
}

impl Side {
    /// The same thing, as the protocol names it.
    pub fn from_wire(layer: lxb_protocol::server::lxb_shell_v1::BehindLayer) -> Self {
        match layer {
            lxb_protocol::server::lxb_shell_v1::BehindLayer::Above => Self::Above,
            _ => Self::Below,
        }
    }
}

/// Draw what the compositor is putting on one side of the shell's own surfaces,
/// small, so a pane of the shell's glass can refract it.
///
/// **The same builders the display's own frame is made of**, called with a
/// smaller scale rather than drawn large and shrunk afterwards. An element
/// carries where it goes and how big it is as a function of the scale it is
/// asked at, so asking for a fraction of one is a picture a fraction of the
/// size, drawn by the GPU in one pass at that size. Drawing it full size and
/// blitting it down would be a second whole composite and an offscreen the size
/// of the display to keep.
///
/// Small on purpose. A pane frosts what it transmits, so what it wants back is
/// something already blurred; the shell chooses how small by how large a buffer
/// it hands over, and a readback it can afford every frame is the whole point.
///
/// Transparent where nothing was drawn, because the shell composites this over
/// the wallpaper it evaluates for itself: what nothing covers must arrive
/// covering nothing.
pub fn behind<R>(
    renderer: &mut R,
    lxb: &Lxb,
    output: &Output,
    side: Side,
    size: Size<i32, Physical>,
) -> anyhow::Result<Shot>
where
    R: Renderer + ImportAll + ImportMem + ExportMem + Offscreen<GlesTexture>,
    R::TextureId: Send + Clone + 'static,
    R::Error: Send + Sync + 'static,
{
    if size.w <= 0 || size.h <= 0 {
        anyhow::bail!("no room in the buffer to draw a picture");
    }
    let shown = crate::screencopy::picture_size(output)
        .ok_or_else(|| anyhow::anyhow!("the display has no size to draw"))?;
    if shown.w <= 0 || shown.h <= 0 {
        anyhow::bail!("the display has no size to draw");
    }
    // How much smaller than the display this picture is, and so the scale every
    // element is asked to place itself at.
    let shrink = f64::min(
        size.w as f64 / shown.w as f64,
        size.h as f64 / shown.h as f64,
    );
    let scale = Scale::from(output.current_scale().fractional_scale() * shrink);

    let elements = crate::render::elements_behind_the_shell(renderer, lxb, output, side, scale);
    shoot(
        renderer,
        size,
        &mut OutputDamageTracker::new(size, scale, Transform::Normal),
        &elements,
        [0.0; 4],
    )
}

/// Draw `elements` into an offscreen buffer `size` pixels across and read the
/// result back into main memory.
///
/// The half every picture shares — [`crate::screencopy`]'s frames as well as
/// these two. `tracker` carries the scale and the orientation to draw at, and
/// is always a fresh one: a picture has no previous frame to be a difference
/// from, so every pixel of it is drawn.
pub fn shoot<R, E>(
    renderer: &mut R,
    size: Size<i32, Physical>,
    tracker: &mut OutputDamageTracker,
    elements: &[E],
    clear: [f32; 4],
) -> anyhow::Result<Shot>
where
    R: Renderer + ImportMem + ExportMem + Offscreen<GlesTexture>,
    R::TextureId: Send + Clone + 'static,
    R::Error: Send + Sync + 'static,
    E: RenderElement<R>,
{
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

    tracker
        .render_output(renderer, &mut framebuffer, 0, elements, clear)
        .map_err(|err| anyhow::anyhow!("could not render the picture: {err:?}"))?;

    let mapping = renderer
        .copy_framebuffer(&framebuffer, Rectangle::from_size(buffer), Fourcc::Abgr8888)
        .map_err(|err| anyhow::anyhow!("could not read the picture back: {err}"))?;
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
