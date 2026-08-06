//! wgpu renderer.
//!
//! Two pipelines draw the interface:
//!
//! * a full-screen pass that draws the animated XMB backdrop in a shader, and
//! * an instanced textured-quad pass that draws every icon and panel from a
//!   single atlas.
//!
//! Text is handled by glyphon on top of that.
//!
//! Two more exist because of the glass. A pane shows what is behind it, so the
//! frame is built in a texture instead of straight on the display: the quads
//! are split into runs, the frame so far is snapshotted between them, and a
//! chain of halvings is blurred off that snapshot for frost to read. The last
//! pipeline copies the finished texture onto the display. Everything that
//! costs is proportional to how much glass is on screen, and a frame with none
//! takes no snapshot at all.

use std::collections::HashMap;
use std::num::NonZeroU64;

use glyphon::{
    Attrs, Buffer as TextBuffer, Cache, Color as TextColor, Family, FontSystem, Metrics,
    Resolution, Shaping, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
    Weight,
};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use wgpu::util::DeviceExt;

use crate::icons::Icon;

/// Edge length, in pixels, of one atlas cell.
const CELL: u32 = 128;

/// A textured or solid rectangle, in physical pixels with the origin top-left.
#[derive(Debug, Clone, Copy)]
pub struct Quad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Atlas slot, or [`SOLID_SLOT`] for a flat colour.
    pub slot: u32,
    /// Multiplied with the sampled texel. For solid quads this *is* the colour.
    pub color: [f32; 4],
    /// Corner radius in pixels. Zero draws the plain rectangle, without even
    /// the edge antialiasing a rounded one needs — a hairline rule must stay
    /// a hairline.
    pub radius: f32,
    /// Which norm the corners are cut to. [`CIRCULAR_CORNER`] is the
    /// quarter-round arc; [`SQUIRCLE_CORNER`] spreads the same bend out along
    /// the edges instead of ending it where the arc meets the straight.
    ///
    /// It only says what shape the corner *is*; [`Self::radius`] still says how
    /// far it reaches. The two together are what turn a square quad with a
    /// half-width radius from a disc into a rounded square.
    pub corner: f32,
    /// Thickness in pixels of an outline instead of a fill: the card frames
    /// and the selection ring, which follow their rounded corners the whole
    /// way round rather than being four straight runs.
    pub border: f32,
    /// Half-width in pixels of a vertical slot cut through the quad's upper
    /// half. Zero for everything but the power glyph, whose ring is broken at
    /// the top; the break cannot be painted over, because the application
    /// behind the overlay shows through anything drawn on it.
    pub notch: f32,
    /// How deep this pane is, in pixels — half the slab's thickness, and the
    /// width of the rounded-over bevel at its rim, which for a real edge are
    /// the same number.
    ///
    /// Non-zero turns the fill into a *material*. The shader treats the quad
    /// as a slab of glass floating over what is behind it and traces a ray
    /// through it: in at the bevel, across the depth, out of the flat
    /// underside, on to the surface below. What comes back is what the pane
    /// shows, stained by `color`. That is why the bevel bends hardest at the
    /// very rim and not at all across the face, and why the bend splits into
    /// colour — one ray per wavelength, as through any thick edge.
    pub thickness: f32,
    /// How softly the wallpaper *behind* this pane is drawn: 0 on the bar,
    /// where the surface paints it sharp, up to 1 in the menu, where what is
    /// behind is the blurred backdrop on the layer below. A pane that guesses
    /// this wrong shows a sharp wallpaper against a blurred one.
    ///
    /// Only reaches the parts of the frame the shell did not draw itself,
    /// which it cannot read back: the backdrop lives on a second Wayland
    /// surface below this one. Everything this surface *has* drawn is read
    /// from the frame, so no guess is involved.
    pub behind: f32,
    /// How much this pane scatters what it transmits: 0 is clear glass, 1 is
    /// deep frost.
    ///
    /// Frost is what makes a pane a *surface* rather than a hole — and what
    /// keeps a label on it legible over anything at all. It is separate from
    /// [`Self::thickness`] because the two are independent in real glass and
    /// in the layout: the sidebar is thick and heavily frosted, the disc
    /// under a chosen icon is just as thick and nearly clear.
    ///
    /// It does need some thickness to be about, though. A pane with none
    /// transmits nothing, and there is nothing to scatter.
    pub frost: f32,
    /// How strongly this pane takes the light: 0 leaves a flat shape, 1 is
    /// glass under the one lamp the whole shell is lit by.
    ///
    /// There is no separate control for the rim, the sheen or the shadow along
    /// the underside. Those are not three effects — they are what a slab of
    /// this shape does with that lamp, and the shader works them out from the
    /// surface rather than being told.
    pub gloss: f32,
    /// The pane's own opacity, multiplied into everything above.
    ///
    /// Separate from the alpha in `color`, which on a glass pane means how
    /// strongly it *stains* the wallpaper rather than how solid it is — a
    /// pane can be deeply tinted and still fading out.
    pub fade: f32,
}

impl Default for Quad {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
            slot: SOLID_SLOT,
            color: [0.0; 4],
            radius: 0.0,
            // The other field whose zero is wrong: a corner cut to no norm at
            // all is not a shape.
            corner: CIRCULAR_CORNER,
            border: 0.0,
            notch: 0.0,
            thickness: 0.0,
            behind: 0.0,
            frost: 0.0,
            gloss: 0.0,
            // Written out because this is the one field whose zero is wrong:
            // every `..Quad::default()` in the shell would draw nothing.
            fade: 1.0,
        }
    }
}

impl Quad {
    /// Whether this quad has to read the frame beneath it to draw itself.
    ///
    /// Exactly the panes the shader traces a ray through: filled, rounded, and
    /// with some depth to trace through. An outline has no interior to see
    /// anything, a plain rectangle is a colour, and a pane with no depth is
    /// only ever lit — its rim reflects, but nothing passes through it.
    ///
    /// This has to agree with the shader. Claiming a pane reads the frame when
    /// it does not costs a snapshot nobody looks at; claiming it does not when
    /// it does hands it the frame as it stood before the thing it is lying on
    /// was drawn.
    fn reads_backdrop(&self) -> bool {
        self.radius > 0.0 && self.border <= 0.0 && self.thickness > 0.0
    }

    fn overlaps(&self, other: &Quad) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }
}

/// One run of quads that can all be drawn against a single snapshot of the
/// frame so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Batch {
    /// One past the last quad in the run.
    end: usize,
    /// Whether anything in it reads that snapshot, and so whether one has to
    /// be taken before the run is drawn.
    reads: bool,
}

/// The most snapshots one frame may take of itself.
///
/// Each costs a copy of the display and a blur chain over it, so this is a
/// budget rather than a limit on correctness: past it, panes share the
/// snapshot the run before them took and refract the frame as it stood
/// earlier in the draw order than it really is.
///
/// Which is a quiet failure, not a loud one — it looks like glass that has
/// stopped frosting. The shell's busiest screen, the power dialog over the
/// guide over the bar, needs seven: the bar's two lit discs each sit on their
/// own bloom, the chips sit on the sidebar, the dialog sits on its scrim and
/// its rows sit on the dialog. Set well above that, because the way this is
/// discovered is by noticing something looking wrong.
pub const MAX_GLASS_BATCHES: usize = 12;

/// How many snapshots a frame made of these quads actually needs, with no
/// budget applied — so that a layout can be asserted to fit inside one.
///
/// Only the layout's own tests ask. The renderer never needs to know, because
/// running out of budget is something it survives.
#[cfg(test)]
pub fn snapshots_needed(quads: &[Quad]) -> usize {
    glass_batches(quads, usize::MAX).len()
}

/// Split the quad stream into runs that can each be drawn against one
/// snapshot of what is already on screen.
///
/// A pane of glass shows what is behind it, and *behind* means everything
/// drawn before it — so a pane cannot share a run with anything it overlaps.
/// Panes that overlap nothing already drawn cost nothing: the whole rank of
/// category discs goes in one run, because no disc is on top of another. It
/// is the power button sitting on the sidebar, and the dialog sitting on
/// both, that have to wait for a fresh snapshot.
fn glass_batches(quads: &[Quad], limit: usize) -> Vec<Batch> {
    let mut batches: Vec<Batch> = Vec::new();
    let mut start = 0;
    let mut reads = false;

    for (index, quad) in quads.iter().enumerate() {
        if !quad.reads_backdrop() {
            continue;
        }
        let covered = quads[start..index].iter().any(|under| under.overlaps(quad));
        if covered && batches.len() + 1 < limit {
            batches.push(Batch { end: index, reads });
            start = index;
        }
        // Whichever run this landed in, that run now needs a snapshot.
        reads = true;
    }

    batches.push(Batch {
        end: quads.len(),
        reads,
    });
    batches
}

/// Open a pass onto the texture the frame is being built in.
///
/// Reopening a pass rather than holding one is what lets a snapshot be taken
/// between two runs of quads; `Load` is what keeps everything the run before
/// it drew.
fn onto_scene<'a>(
    encoder: &'a mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    load: wgpu::LoadOp<wgpu::Color>,
    label: &str,
) -> wgpu::RenderPass<'a> {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some(label),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load,
                store: wgpu::StoreOp::Store,
            },
            depth_slice: None,
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    })
}

/// A whole texture's top level, as the source or destination of a copy.
fn whole(texture: &wgpu::Texture) -> wgpu::TexelCopyTextureInfo<'_> {
    wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: 0,
        origin: wgpu::Origin3d::ZERO,
        aspect: wgpu::TextureAspect::All,
    }
}

/// The corner every rounded rectangle has had since rounded rectangles
/// existed: a quarter-round arc, which meets the straight edge at a point
/// where the curvature falls from all of it to none of it at once.
pub const CIRCULAR_CORNER: f32 = 2.0;

/// A squircle's corner — the fourth-power superellipse. The same bend, spread
/// out along the edges rather than stopping dead where the arc ends.
///
/// On a shape whose radius is its own half-width, which is to say on a circle,
/// this is the whole difference between a disc and a rounded square.
pub const SQUIRCLE_CORNER: f32 = 4.0;

/// The reserved opaque-white slot used to draw untextured rectangles.
pub const SOLID_SLOT: u32 = 0;

/// A soft radial glow, generated at startup. Tinted and pulsed, it is what
/// makes the selected entry unmistakable — the XMB highlight.
pub const GLOW_SLOT: u32 = 1;

/// Atlas cells taken by the procedural sprites above; icons start after them.
const RESERVED_SLOTS: u32 = 2;

/// The shell's typeface, carried in the binary rather than looked up.
///
/// A session shell draws its first frame before anything about the machine is
/// guaranteed — including which fonts are installed. Asking for `sans-serif`
/// gets whatever fontconfig happens to resolve, which is a different face on
/// every distribution and no face at all on a minimal install; every size and
/// gap in the layout was chosen against *this* one. So the two faces the shell
/// actually uses travel with it.
///
/// Only two: everything here is drawn at [`Weight::NORMAL`] or
/// [`Weight::BOLD`], and a face nothing asks for is 160KB of binary that never
/// renders a glyph.
const UI_FONT: &str = "Roboto";
const UI_FONT_REGULAR: &[u8] = include_bytes!("../../../font/Roboto/static/Roboto-Regular.ttf");
const UI_FONT_BOLD: &[u8] = include_bytes!("../../../font/Roboto/static/Roboto-Bold.ttf");

/// Where a run sits inside its `max_width` box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

/// Everything about a [`Text`] that affects its shaping and layout.
///
/// Position and colour are deliberately excluded: moving or recolouring a run
/// does not require re-shaping it.
#[derive(PartialEq)]
struct TextKey {
    content: String,
    size: u32,
    max_width: u32,
    bold: bool,
    align: TextAlign,
}

impl TextKey {
    fn of(text: &Text) -> Self {
        Self {
            content: text.content.clone(),
            // Sub-pixel jitter in a size must not invalidate the shaping.
            size: text.size.to_bits(),
            max_width: text.max_width.to_bits(),
            bold: text.bold,
            align: text.align,
        }
    }
}

/// A run of text to draw.
pub struct Text {
    pub content: String,
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub color: [f32; 4],
    pub bold: bool,
    /// Wrapping / clipping width in pixels.
    pub max_width: f32,
    pub align: TextAlign,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Instance {
    rect: [f32; 4],
    uv: [f32; 4],
    color: [f32; 4],
    /// Corner radius, outline thickness and notch half-width in pixels, then
    /// how much the pane frosts what it transmits.
    shape: [f32; 4],
    /// Slab depth in pixels, the blur of what is behind, gloss and opacity.
    material: [f32; 4],
    /// The norm the corners are cut to. On its own rather than packed into
    /// the four above because they are full — and one float is cheaper than
    /// the three wasted ones a fresh vector would carry.
    corner: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    resolution: [f32; 2],
    time: f32,
    /// 0.0 renders the backdrop sharp; 1.0 fully softened and dimmed.
    blur: f32,
    /// Pixel rectangle the background is squeezed into, as x, y, w, h;
    /// everything outside it is left transparent. All zeros fills the
    /// surface.
    window_rect: [f32; 4],
    /// Corner radius in pixels, the blur to draw the corner covers with, and
    /// how many of them are in use. The fourth is padding.
    params: [f32; 4],
    /// The wallpaper's palette: the gradient as two pairs the mood drifts
    /// between, then the active accent at its normal, soft and deep rungs, and
    /// the glow behind the cross point. Here rather than in the shader so the
    /// theme is one Rust value.
    sky: [[f32; 4]; 4],
    accent: [[f32; 4]; 3],
    glow: [f32; 4],
    covers: [[f32; 4]; MAX_COVERS],
}

/// How many card corners one pass can cover. The column shows at most three
/// cards whole and two more cut off by the screen edges.
pub const MAX_COVERS: usize = 6;

/// How many rungs the backdrop's blur chain has, counting the sharp copy at
/// the top.
///
/// The shader names the deepest rung it may ask for as `BACKDROP_LEVELS`, one
/// less than this, and the two have to agree: a pane asking for a rung that
/// was never rendered samples whatever the last frame left there.
const BACKDROP_MIPS: u32 = 5;

/// How a surface's backdrop pass should be drawn.
#[derive(Debug, Clone, Copy)]
pub struct Backdrop {
    pub blur: f32,
    /// Where to draw it. All zeros is the whole surface; a rectangle draws
    /// the background scaled into it — the start screen as a miniature in
    /// its overview card, and every size in between while it flies.
    pub window_rect: [f32; 4],
    /// Corner radius in pixels for `window_rect` and for the covers.
    pub corner_radius: f32,
    /// Rectangles whose square corners must be hidden: the live windows the
    /// compositor draws in the overview's cards. This pass repaints just the
    /// slivers outside their rounded corners with the background that is
    /// behind them, which is the only way to round a window this surface
    /// does not own. `cover_blur` is what the backdrop under them is drawn
    /// with, so the repaint is indistinguishable from it.
    pub covers: [[f32; 4]; MAX_COVERS],
    pub cover_count: u32,
    pub cover_blur: f32,
    /// Opacity of the background drawn into `window_rect`.
    ///
    /// The start screen's card is a miniature of a *whole display* — its
    /// wallpaper comes from this pass, not from the scene, so fading the
    /// scene alone leaves the card's wallpaper at full strength on top of an
    /// application the compositor has not finished shrinking. The corner
    /// covers are never faded: they are repairs to what is already on screen.
    pub fade: f32,
}

impl Default for Backdrop {
    fn default() -> Self {
        Self {
            blur: 0.0,
            window_rect: [0.0; 4],
            corner_radius: 0.0,
            covers: [[0.0; 4]; MAX_COVERS],
            cover_count: 0,
            cover_blur: 0.0,
            fade: 1.0,
        }
    }
}

/// Everything shared by every display: the device, the pipelines, the icon
/// atlas and the font system.
///
/// Splitting this from [`Target`] is what makes a second monitor nearly free —
/// it costs one more swapchain rather than a second copy of the atlas, the
/// shaders and the shaped glyph cache.
pub struct Gpu {
    /// Kept so further displays can have surfaces created for them.
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    format: wgpu::TextureFormat,

    quad_pipeline: wgpu::RenderPipeline,
    background_pipeline: wgpu::RenderPipeline,
    /// One rung of the backdrop's blur chain from the one above it.
    downsample_pipeline: wgpu::RenderPipeline,
    /// The finished frame, from the texture it was built in onto the display.
    blit_pipeline: wgpu::RenderPipeline,
    globals_layout: wgpu::BindGroupLayout,
    /// One texture and one sampler: what both offscreen passes read, and what
    /// binds the backdrop to the quad pipeline.
    sample_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    atlas_bind_group: wgpu::BindGroup,

    /// Icon name to atlas slot.
    slots: HashMap<String, u32>,
    atlas_cells_per_row: u32,

    font_system: FontSystem,
    swash_cache: SwashCache,
    text_atlas: TextAtlas,
    text_cache: Cache,
}

/// The two textures a display's frame is actually built in.
///
/// Nothing is drawn straight to the swapchain any more. Glass has to be able
/// to read what is already underneath it, a swapchain image cannot be read
/// while it is being drawn into, and a frame that refracts a *guess* at what
/// is underneath is the thing this exists to stop being necessary.
///
/// It costs about two and a half display's worth of memory per display — one
/// for the frame, one and a third for the snapshot and its blur chain.
struct Offscreen {
    /// The frame as it is being built, and the bind group that reads it for
    /// the final copy onto the display.
    scene: wgpu::Texture,
    scene_view: wgpu::TextureView,
    scene_source: wgpu::BindGroup,
    /// A snapshot of `scene`, taken whenever a pane is about to refract it,
    /// with a chain of ever-blurrier halvings below it for frost.
    backdrop: wgpu::Texture,
    /// It, bound as what the glass sees.
    backdrop_source: wgpu::BindGroup,
    /// One view per rung, to render the chain into, and one bind group per
    /// rung, to read the rung above while doing so.
    rungs: Vec<wgpu::TextureView>,
    rung_sources: Vec<wgpu::BindGroup>,
    width: u32,
    height: u32,
}

/// One display's swapchain and the per-display state that goes with it.
pub struct Target {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    offscreen: Offscreen,
    /// Resolution and time, which differ per display.
    globals_buffer: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,
    instance_buffer: wgpu::Buffer,
    instance_capacity: u64,
    viewport: Viewport,
    text_renderer: TextRenderer,
    /// Shaped text buffers, reused frame to frame.
    ///
    /// cosmic-text caches shaping and layout inside the buffer, so building a
    /// fresh one per run per frame would re-shape unchanged text ~1000 times a
    /// second. Each slot remembers what it was last built from and only
    /// re-shapes when that changes. Displays of different sizes lay text out
    /// differently, so the pool belongs to the display rather than the device.
    text_buffers: Vec<(TextKey, TextBuffer)>,
}

impl Gpu {
    /// Bring up the device against a first Wayland surface, and return that
    /// surface's render target with it.
    ///
    /// # Safety
    ///
    /// `display` and `surface` must be valid `wl_display` / `wl_surface`
    /// pointers that outlive the returned renderer.
    pub unsafe fn new(
        display: *mut std::ffi::c_void,
        surface: *mut std::ffi::c_void,
        width: u32,
        height: u32,
        icons: Vec<(String, Icon)>,
    ) -> anyhow::Result<(Self, Target)> {
        let mut instance_descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        instance_descriptor.backends = wgpu::Backends::VULKAN | wgpu::Backends::GL;
        let instance = wgpu::Instance::new(instance_descriptor);

        let display_handle = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            std::ptr::NonNull::new(display).ok_or_else(|| anyhow::anyhow!("null wl_display"))?,
        ));
        let window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(
            std::ptr::NonNull::new(surface).ok_or_else(|| anyhow::anyhow!("null wl_surface"))?,
        ));

        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(display_handle),
                raw_window_handle: window_handle,
            })?
        };

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
            apply_limit_buckets: false,
        }))
        .map_err(|e| anyhow::anyhow!("no suitable GPU adapter: {e}"))?;

        tracing::info!(adapter = ?adapter.get_info().name, "using GPU");

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("linboard-xmb"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .map_err(|e| anyhow::anyhow!("could not open GPU device: {e}"))?;

        let capabilities = surface.get_capabilities(&adapter);
        // Prefer a straightforward sRGB target; the shader writes linear values.
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|f| *f == wgpu::TextureFormat::Bgra8UnormSrgb)
            .or_else(|| capabilities.formats.first().copied())
            .ok_or_else(|| anyhow::anyhow!("surface offers no formats"))?;

        let config = surface_config(&capabilities, format, width, height);
        surface.configure(&device, &config);

        // --- atlas -------------------------------------------------------
        let (atlas_texture, slots, cells_per_row) = build_atlas(&device, &queue, icons)?;
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("atlas sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        // --- uniforms ----------------------------------------------------
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(std::mem::size_of::<Globals>() as u64),
                },
                count: None,
            }],
        });
        // Shared by everything that reads one texture: the blur chain, the
        // final copy, and the glass reading what is behind it.
        let sample_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sample layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        // Clamped at the edges: a pane at the corner of the display refracts
        // past it, and a wrapped sample would show the far side of the screen
        // squeezed into its rim.
        let frame_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("frame sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });

        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("atlas layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas"),
            layout: &atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        // --- pipelines ---------------------------------------------------
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("xmb shaders"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders.wgsl").into()),
        });

        let background_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("background layout"),
            bind_group_layouts: &[Some(&globals_layout)],
            immediate_size: 0,
        });
        let background_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("background"),
            layout: Some(&background_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_background"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_background"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // The offscreen passes, in their own module: they bind a texture where
        // the shell's own shaders bind the globals, and one module cannot
        // declare two different things at the same slot.
        let offscreen_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("offscreen shaders"),
            source: wgpu::ShaderSource::Wgsl(include_str!("offscreen.wgsl").into()),
        });
        let offscreen_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("offscreen layout"),
            bind_group_layouts: &[Some(&sample_layout)],
            immediate_size: 0,
        });
        let offscreen_pipeline = |label: &str, entry: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&offscreen_layout),
                vertex: wgpu::VertexState {
                    module: &offscreen_shader,
                    entry_point: Some("vs_fullscreen"),
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &offscreen_shader,
                    entry_point: Some(entry),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        // Both replace what is there rather than blending into
                        // it: a blur rung and a finished frame are each the
                        // whole answer, premultiplied alpha and all.
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let downsample_pipeline = offscreen_pipeline("downsample", "fs_downsample");
        let blit_pipeline = offscreen_pipeline("blit", "fs_blit");

        let quad_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quad layout"),
            bind_group_layouts: &[
                Some(&globals_layout),
                Some(&atlas_layout),
                Some(&sample_layout),
            ],
            immediate_size: 0,
        });
        let quad_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quads"),
            layout: Some(&quad_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_quad"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Instance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![
                        0 => Float32x4,
                        1 => Float32x4,
                        2 => Float32x4,
                        3 => Float32x4,
                        4 => Float32x4,
                        5 => Float32,
                    ],
                })],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_quad"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        // --- text --------------------------------------------------------
        // `FontSystem::new` already scans and loads the system fonts; asking
        // again would append every face a second time.
        let mut font_system = FontSystem::new();
        // The system's faces stay loaded underneath the shell's own, because
        // they are the fallback chain: Roboto covers no CJK, and an
        // application whose title is in Japanese still has to have a title.
        for face in [UI_FONT_REGULAR, UI_FONT_BOLD] {
            font_system.db_mut().load_font_data(face.to_vec());
        }
        let swash_cache = SwashCache::new();
        let text_cache = Cache::new(&device);
        let mut text_atlas = TextAtlas::new(&device, &queue, &text_cache, format);

        let target = Target::new(
            Shared {
                device: &device,
                globals_layout: &globals_layout,
                sample_layout: &sample_layout,
                sampler: &frame_sampler,
                text_cache: &text_cache,
            },
            &mut text_atlas,
            surface,
            config,
        );

        Ok((
            Self {
                instance,
                adapter,
                device,
                queue,
                format,
                quad_pipeline,
                background_pipeline,
                downsample_pipeline,
                blit_pipeline,
                globals_layout,
                sample_layout,
                sampler: frame_sampler,
                atlas_bind_group,
                slots,
                atlas_cells_per_row: cells_per_row,
                font_system,
                swash_cache,
                text_atlas,
                text_cache,
            },
            target,
        ))
    }

    /// Add a display.
    ///
    /// # Safety
    ///
    /// `surface` must be a valid `wl_surface` pointer that outlives the
    /// returned target.
    pub unsafe fn add_target(
        &mut self,
        display: *mut std::ffi::c_void,
        surface: *mut std::ffi::c_void,
        width: u32,
        height: u32,
    ) -> anyhow::Result<Target> {
        let display_handle = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            std::ptr::NonNull::new(display).ok_or_else(|| anyhow::anyhow!("null wl_display"))?,
        ));
        let window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(
            std::ptr::NonNull::new(surface).ok_or_else(|| anyhow::anyhow!("null wl_surface"))?,
        ));

        let surface = unsafe {
            self.instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: Some(display_handle),
                    raw_window_handle: window_handle,
                })?
        };

        let capabilities = surface.get_capabilities(&self.adapter);
        if !capabilities.formats.contains(&self.format) {
            anyhow::bail!("display does not support the format the others use");
        }
        let config = surface_config(&capabilities, self.format, width, height);
        surface.configure(&self.device, &config);

        Ok(Target::new(
            Shared {
                device: &self.device,
                globals_layout: &self.globals_layout,
                sample_layout: &self.sample_layout,
                sampler: &self.sampler,
                text_cache: &self.text_cache,
            },
            &mut self.text_atlas,
            surface,
            config,
        ))
    }

    /// Atlas slot for an icon name, if it was loaded.
    pub fn slot(&self, name: &str) -> Option<u32> {
        self.slots.get(name).copied()
    }

    /// Draw one frame.
    ///
    /// `backdrop` is the animated background pass: how blurred, and which
    /// region of the surface it fills. `None` leaves the surface transparent
    /// wherever the scene does not paint — the overlay drawn over a running
    /// application, or a surface whose backdrop lives on another surface.
    pub fn render(
        &mut self,
        target: &mut Target,
        quads: &[Quad],
        texts: &[Text],
        time: f32,
        backdrop: Option<Backdrop>,
    ) -> anyhow::Result<()> {
        let params = backdrop.unwrap_or_default();
        let theme = crate::theme::theme();
        let covers_surface = backdrop.is_some_and(|backdrop| backdrop.window_rect == [0.0; 4]);
        self.queue.write_buffer(
            &target.globals_buffer,
            0,
            bytemuck::bytes_of(&Globals {
                resolution: [target.config.width as f32, target.config.height as f32],
                time,
                blur: params.blur,
                window_rect: params.window_rect,
                params: [
                    params.corner_radius,
                    params.cover_blur,
                    params.cover_count.min(MAX_COVERS as u32) as f32,
                    params.fade,
                ],
                sky: [
                    theme.sky[0].a(1.0),
                    theme.sky[1].a(1.0),
                    theme.sky[2].a(1.0),
                    theme.sky[3].a(1.0),
                ],
                accent: [
                    theme.accent.a(1.0),
                    theme.accent_soft.a(1.0),
                    theme.accent_deep.a(1.0),
                ],
                glow: theme.glow.a(1.0),
                covers: params.covers,
            }),
        );

        let instances: Vec<Instance> = quads
            .iter()
            .map(|q| Instance {
                rect: [q.x, q.y, q.w, q.h],
                uv: self.uv_for(q.slot),
                color: q.color,
                shape: [q.radius, q.border, q.notch, q.frost],
                material: [q.thickness, q.behind, q.gloss, q.fade],
                corner: q.corner,
            })
            .collect();

        target.ensure_instance_capacity(&self.device, instances.len() as u64);
        if !instances.is_empty() {
            self.queue
                .write_buffer(&target.instance_buffer, 0, bytemuck::cast_slice(&instances));
        }

        // Text has to be laid out before the render pass borrows everything.
        target.viewport.update(
            &self.queue,
            Resolution {
                width: target.config.width,
                height: target.config.height,
            },
        );

        shape_texts(&mut self.font_system, &mut target.text_buffers, texts);

        let areas: Vec<TextArea<'_>> = target
            .text_buffers
            .iter()
            .zip(texts)
            .map(|((_, buffer), text)| TextArea {
                buffer,
                left: text.x,
                top: text.y,
                scale: 1.0,
                bounds: TextBounds {
                    left: text.x as i32,
                    top: text.y as i32,
                    right: (text.x + text.max_width) as i32,
                    bottom: (text.y + text.size * 2.0) as i32,
                },
                default_color: TextColor::rgba(
                    (text.color[0] * 255.0) as u8,
                    (text.color[1] * 255.0) as u8,
                    (text.color[2] * 255.0) as u8,
                    (text.color[3] * 255.0) as u8,
                ),
                custom_glyphs: &[],
            })
            .collect();

        target.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.text_atlas,
            &target.viewport,
            areas,
            &mut self.swash_cache,
        )?;

        let frame = match target.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            // The surface can go stale across a resize; reconfigure and skip
            // this frame rather than tearing the whole shell down.
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                target.surface.configure(&self.device, &target.config);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(())
            }
            other => return Err(anyhow::anyhow!("could not acquire frame: {other:?}")),
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });

        let off = &target.offscreen;

        {
            // Only a backdrop covering the whole surface may clear to black:
            // one drawn into a rectangle has to leave the rest see-through, or
            // the miniature start screen would arrive with the display painted
            // around it.
            let clear = wgpu::LoadOp::Clear(if covers_surface {
                wgpu::Color::BLACK
            } else {
                wgpu::Color::TRANSPARENT
            });
            let mut pass = onto_scene(&mut encoder, &off.scene_view, clear, "background");
            if backdrop.is_some() {
                pass.set_pipeline(&self.background_pipeline);
                pass.set_bind_group(0, &target.globals_bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }

        let stride = std::mem::size_of::<Instance>() as u64;
        let mut drawn = 0;
        for batch in glass_batches(quads, MAX_GLASS_BATCHES) {
            if batch.reads {
                self.snapshot(&mut encoder, off);
            }
            if batch.end == drawn {
                continue;
            }
            let mut pass = onto_scene(&mut encoder, &off.scene_view, wgpu::LoadOp::Load, "quads");
            pass.set_pipeline(&self.quad_pipeline);
            pass.set_bind_group(0, &target.globals_bind_group, &[]);
            pass.set_bind_group(1, &self.atlas_bind_group, &[]);
            pass.set_bind_group(2, &off.backdrop_source, &[]);
            // Sliced rather than drawn from an instance offset: a non-zero
            // first instance is an extension on some of the backends this
            // runs on, and rebinding the buffer costs nothing.
            pass.set_vertex_buffer(0, target.instance_buffer.slice(drawn as u64 * stride..));
            pass.draw(0..6, 0..(batch.end - drawn) as u32);
            drawn = batch.end;
        }

        {
            let mut pass = onto_scene(&mut encoder, &off.scene_view, wgpu::LoadOp::Load, "text");
            target
                .text_renderer
                .render(&self.text_atlas, &target.viewport, &mut pass)?;
        }

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("present"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // The copy covers every pixel and does not blend, so
                        // what was here is irrelevant — and discarding it is
                        // cheaper than fetching it.
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.blit_pipeline);
            pass.set_bind_group(0, &off.scene_source, &[]);
            pass.draw(0..3, 0..1);
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        self.text_atlas.trim();

        Ok(())
    }

    /// Hand the frame as it currently stands to the glass about to be drawn
    /// over it, together with the chain of blurred copies frost reads.
    ///
    /// The copy is of the whole display rather than of the panes that asked
    /// for it. A pane refracts and scatters *outwards* — it reaches a good way
    /// past its own rim — and working out how far, per pane, to save a copy
    /// that a GPU does in well under a millisecond is the wrong trade.
    fn snapshot(&self, encoder: &mut wgpu::CommandEncoder, off: &Offscreen) {
        encoder.copy_texture_to_texture(
            whole(&off.scene),
            whole(&off.backdrop),
            wgpu::Extent3d {
                width: off.width,
                height: off.height,
                depth_or_array_layers: 1,
            },
        );

        for rung in 1..off.rungs.len() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("backdrop blur"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &off.rungs[rung],
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.downsample_pipeline);
            pass.set_bind_group(0, &off.rung_sources[rung - 1], &[]);
            pass.draw(0..3, 0..1);
        }
    }

    /// Texture coordinates of an atlas slot.
    fn uv_for(&self, slot: u32) -> [f32; 4] {
        let per_row = self.atlas_cells_per_row.max(1);
        let col = slot % per_row;
        let row = slot / per_row;
        let step = 1.0 / per_row as f32;

        if slot == SOLID_SLOT {
            // Sample the middle of the white cell so filtering never bleeds in
            // a neighbouring icon's edge.
            let c = step * 0.5;
            return [c, c, c, c];
        }

        // Keep the sample footprint well inside the cell: bilinear filtering
        // reaches half a texel beyond the sampled area, so anything closer to
        // the border blends in the neighbouring cell. The glow sits next to
        // the opaque-white solid cell, where that bleed used to draw a bright
        // hairline along the edge of every (heavily magnified) glow quad.
        let inset = step * (1.5 / CELL as f32);
        [
            col as f32 * step + inset,
            row as f32 * step + inset,
            (col + 1) as f32 * step - inset,
            (row + 1) as f32 * step - inset,
        ]
    }
}

impl Offscreen {
    fn new(
        device: &wgpu::Device,
        layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let scene = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });

        // As many halvings as the display has room for, so a small nested
        // display does not ask for a rung one pixel wide.
        let levels = BACKDROP_MIPS.min(width.min(height).ilog2().max(1));
        let backdrop = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("backdrop"),
            size: extent,
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let rung_view = |level: u32| {
            backdrop.create_view(&wgpu::TextureViewDescriptor {
                label: Some("backdrop rung"),
                base_mip_level: level,
                mip_level_count: Some(1),
                ..Default::default()
            })
        };
        let source = |view: &wgpu::TextureView, label: &str| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some(label),
                layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            })
        };

        let rungs: Vec<wgpu::TextureView> = (0..levels).map(rung_view).collect();
        let rung_sources = rungs
            .iter()
            .map(|view| source(view, "backdrop rung"))
            .collect();

        let scene_view = scene.create_view(&wgpu::TextureViewDescriptor::default());
        let scene_source = source(&scene_view, "scene");
        let backdrop_source = source(
            &backdrop.create_view(&wgpu::TextureViewDescriptor::default()),
            "backdrop",
        );

        Self {
            scene,
            scene_view,
            scene_source,
            backdrop,
            backdrop_source,
            rungs,
            rung_sources,
            width,
            height,
        }
    }
}

/// The device-side pieces every display's target is built against — the same
/// values for all of them, which is the point of splitting [`Target`] off
/// [`Gpu`] in the first place.
struct Shared<'a> {
    device: &'a wgpu::Device,
    globals_layout: &'a wgpu::BindGroupLayout,
    sample_layout: &'a wgpu::BindGroupLayout,
    sampler: &'a wgpu::Sampler,
    text_cache: &'a Cache,
}

impl Target {
    fn new(
        shared: Shared<'_>,
        text_atlas: &mut TextAtlas,
        surface: wgpu::Surface<'static>,
        config: wgpu::SurfaceConfiguration,
    ) -> Self {
        let Shared {
            device,
            globals_layout,
            sample_layout,
            sampler,
            text_cache,
        } = shared;
        let globals_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("globals"),
            contents: bytemuck::bytes_of(&Globals {
                resolution: [config.width as f32, config.height as f32],
                time: 0.0,
                blur: 0.0,
                window_rect: [0.0; 4],
                params: [0.0; 4],
                // Placeholders: every frame rewrites the whole block from the
                // theme before anything is drawn from it.
                sky: [[0.0; 4]; 4],
                accent: [[0.0; 4]; 3],
                glow: [0.0; 4],
                covers: [[0.0; 4]; MAX_COVERS],
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });

        let instance_capacity = 1024;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: instance_capacity * std::mem::size_of::<Instance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let offscreen = Offscreen::new(
            device,
            sample_layout,
            sampler,
            config.format,
            config.width,
            config.height,
        );

        Self {
            surface,
            config,
            offscreen,
            globals_buffer,
            globals_bind_group,
            instance_buffer,
            instance_capacity,
            viewport: Viewport::new(device, text_cache),
            text_renderer: TextRenderer::new(
                text_atlas,
                device,
                wgpu::MultisampleState::default(),
                None,
            ),
            text_buffers: Vec::new(),
        }
    }

    pub fn resize(&mut self, gpu: &Gpu, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        if width == self.config.width && height == self.config.height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&gpu.device, &self.config);
        self.offscreen = Offscreen::new(
            &gpu.device,
            &gpu.sample_layout,
            &gpu.sampler,
            self.config.format,
            width,
            height,
        );
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    fn ensure_instance_capacity(&mut self, device: &wgpu::Device, needed: u64) {
        if needed <= self.instance_capacity {
            return;
        }
        let capacity = needed.next_power_of_two();
        self.instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: capacity * std::mem::size_of::<Instance>() as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.instance_capacity = capacity;
    }
}

/// Bring a target's buffer pool in line with `texts`, re-shaping only the runs
/// whose content or styling actually changed.
fn shape_texts(
    font_system: &mut FontSystem,
    pool: &mut Vec<(TextKey, TextBuffer)>,
    texts: &[Text],
) {
    pool.truncate(texts.len());

    for (index, text) in texts.iter().enumerate() {
        let key = TextKey::of(text);

        if let Some((existing, _)) = pool.get(index) {
            if *existing == key {
                continue;
            }
        }

        let mut buffer = TextBuffer::new(font_system, Metrics::new(text.size, text.size * 1.25));
        buffer.set_size(Some(text.max_width), Some(text.size * 2.0));
        let attrs = Attrs::new()
            .family(Family::Name(UI_FONT))
            .weight(if text.bold {
                Weight::BOLD
            } else {
                Weight::NORMAL
            });
        buffer.set_text(&text.content, &attrs, Shaping::Advanced, None);
        let align = match text.align {
            TextAlign::Left => None,
            TextAlign::Center => Some(glyphon::cosmic_text::Align::Center),
            TextAlign::Right => Some(glyphon::cosmic_text::Align::Right),
        };
        for line in buffer.lines.iter_mut() {
            line.set_align(align);
        }
        buffer.shape_until_scroll(font_system, false);

        match pool.get_mut(index) {
            Some(slot) => *slot = (key, buffer),
            None => pool.push((key, buffer)),
        }
    }
}

/// Swapchain settings shared by every display the shell draws on.
fn surface_config(
    capabilities: &wgpu::SurfaceCapabilities,
    format: wgpu::TextureFormat,
    width: u32,
    height: u32,
) -> wgpu::SurfaceConfiguration {
    wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        color_space: wgpu::SurfaceColorSpace::Srgb,
        width: width.max(1),
        height: height.max(1),
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode: pick_alpha_mode(&capabilities.alpha_modes),
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    }
}

/// Choose how the compositor should interpret the surface's alpha channel.
///
/// The guide overlay needs the running application to show through, which only
/// works if the compositor blends. Both pipelines here write with
/// `ALPHA_BLENDING` over a transparent clear, so their output is already
/// premultiplied; `PostMultiplied` is the fallback for drivers that offer
/// nothing else, and `Opaque` costs only the see-through effect.
fn pick_alpha_mode(available: &[wgpu::CompositeAlphaMode]) -> wgpu::CompositeAlphaMode {
    for preferred in [
        wgpu::CompositeAlphaMode::PreMultiplied,
        wgpu::CompositeAlphaMode::PostMultiplied,
        wgpu::CompositeAlphaMode::Inherit,
    ] {
        if available.contains(&preferred) {
            return preferred;
        }
    }

    tracing::warn!(
        ?available,
        "surface cannot be blended; the guide overlay will not be see-through"
    );
    available
        .first()
        .copied()
        .unwrap_or(wgpu::CompositeAlphaMode::Auto)
}

/// Pack every icon into one texture, sized to fit.
///
/// Slot 0 is filled with opaque white so the same pipeline can draw untextured
/// rectangles by tinting it, and slot 1 holds the procedural selection glow.
fn build_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    icons: Vec<(String, Icon)>,
) -> anyhow::Result<(wgpu::Texture, HashMap<String, u32>, u32)> {
    let needed = icons.len() as u32 + RESERVED_SLOTS;
    let mut cells_per_row = (needed as f64).sqrt().ceil() as u32;
    cells_per_row = cells_per_row.max(2);

    let max_dim = device.limits().max_texture_dimension_2d;
    let max_cells_per_row = (max_dim / CELL).max(1);
    if cells_per_row > max_cells_per_row {
        tracing::warn!(
            icons = icons.len(),
            "too many icons for one atlas; some will be drawn without one"
        );
        cells_per_row = max_cells_per_row;
    }

    let dimension = cells_per_row * CELL;
    let mut pixels = vec![0u8; (dimension * dimension * 4) as usize];

    // Slot 0: opaque white.
    for y in 0..CELL {
        for x in 0..CELL {
            let offset = ((y * dimension + x) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }

    // Slot 1: a white radial glow, gaussian towards the edge with a hard
    // fade-out ring so bilinear filtering never picks up a neighbouring cell.
    let glow_x = (GLOW_SLOT % cells_per_row) * CELL;
    let glow_y = (GLOW_SLOT / cells_per_row) * CELL;
    let half = (CELL as f32 - 1.0) / 2.0;
    for y in 0..CELL {
        for x in 0..CELL {
            let dx = (x as f32 - half) / half;
            let dy = (y as f32 - half) / half;
            let r = (dx * dx + dy * dy).sqrt();
            let falloff = (-5.5 * r * r).exp();
            let edge = ((1.0 - r) / 0.12).clamp(0.0, 1.0);
            let alpha = (falloff * edge * 255.0).round() as u8;
            let offset = (((glow_y + y) * dimension + glow_x + x) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&[255, 255, 255, alpha]);
        }
    }

    let capacity = cells_per_row * cells_per_row;
    let mut slots = HashMap::new();

    for (index, (name, icon)) in icons.into_iter().enumerate() {
        let slot = index as u32 + RESERVED_SLOTS;
        if slot >= capacity {
            break;
        }
        let cell_x = (slot % cells_per_row) * CELL;
        let cell_y = (slot / cells_per_row) * CELL;

        // Icons are decoded at CELL already, but guard against a short buffer.
        let size = icon.size.min(CELL);
        for y in 0..size {
            let src = (y * icon.size * 4) as usize;
            let dst = (((cell_y + y) * dimension + cell_x) * 4) as usize;
            let len = (size * 4) as usize;
            if src + len <= icon.rgba.len() && dst + len <= pixels.len() {
                pixels[dst..dst + len].copy_from_slice(&icon.rgba[src..src + len]);
            }
        }
        slots.insert(name, slot);
    }

    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("icon atlas"),
        size: wgpu::Extent3d {
            width: dimension,
            height: dimension,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });

    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(dimension * 4),
            rows_per_image: Some(dimension),
        },
        wgpu::Extent3d {
            width: dimension,
            height: dimension,
            depth_or_array_layers: 1,
        },
    );

    tracing::debug!(
        dimension,
        cells_per_row,
        icons = slots.len(),
        "built icon atlas"
    );
    Ok((texture, slots, cells_per_row))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pane of glass, `size` across, at `x`.
    fn pane(x: f32, size: f32) -> Quad {
        Quad {
            x,
            y: 0.0,
            w: size,
            h: size,
            radius: size * 0.5,
            thickness: 8.0,
            frost: 0.5,
            ..Quad::default()
        }
    }

    /// Anything drawn on top of one that does not itself read the frame.
    fn opaque(x: f32, size: f32) -> Quad {
        Quad {
            x,
            y: 0.0,
            w: size,
            h: size,
            ..Quad::default()
        }
    }

    fn ends(batches: &[Batch]) -> Vec<usize> {
        batches.iter().map(|batch| batch.end).collect()
    }

    /// The common case, and the one the budget exists for: a rank of panes
    /// side by side, none on top of another, all of which can look at the same
    /// snapshot. The bar draws eight of these and must not cost eight copies
    /// of the display.
    #[test]
    fn panes_that_do_not_overlap_share_one_snapshot() {
        let row: Vec<Quad> = (0..8).map(|i| pane(i as f32 * 120.0, 100.0)).collect();
        let batches = glass_batches(&row, MAX_GLASS_BATCHES);

        assert_eq!(ends(&batches), vec![8]);
        assert!(batches[0].reads, "they do still need one");
    }

    /// And the case it exists to get right: a pane resting on a pane has to
    /// see the pane under it, which means waiting for a snapshot taken after
    /// it was drawn.
    #[test]
    fn a_pane_resting_on_another_waits_for_a_fresh_snapshot() {
        // A sidebar, a chip on it, an icon on the chip, a button on the
        // sidebar well clear of both.
        let quads = vec![
            pane(0.0, 300.0),
            pane(20.0, 60.0),
            opaque(30.0, 40.0),
            pane(200.0, 60.0),
        ];
        let batches = glass_batches(&quads, MAX_GLASS_BATCHES);

        assert_eq!(ends(&batches), vec![1, 4]);
        assert!(batches.iter().all(|batch| batch.reads));
    }

    /// Icons, labels and scrims do not read the frame, so they never cost a
    /// snapshot however many of them there are or whatever they land on.
    #[test]
    fn only_glass_asks_for_a_snapshot() {
        let quads: Vec<Quad> = (0..20).map(|i| opaque(i as f32, 500.0)).collect();
        let batches = glass_batches(&quads, MAX_GLASS_BATCHES);

        assert_eq!(ends(&batches), vec![20]);
        assert!(!batches[0].reads, "and a frame with no glass takes none");
    }

    /// Past the budget the runs stop being split. Panes then refract the frame
    /// as it stood slightly earlier in the draw order, which is wrong but
    /// bounded; an unbounded stack of glass would cost a copy of the display
    /// apiece.
    #[test]
    fn the_budget_caps_how_many_snapshots_a_frame_can_cost() {
        let stack: Vec<Quad> = (0..40).map(|i| pane(i as f32, 400.0)).collect();
        let batches = glass_batches(&stack, MAX_GLASS_BATCHES);

        assert_eq!(batches.len(), MAX_GLASS_BATCHES);
        assert_eq!(batches.last().map(|batch| batch.end), Some(40));
    }

    /// Whatever the splitting decides, every quad is drawn once, in the order
    /// it was given. The runs are consecutive slices of the stream, not a
    /// reordering of it.
    #[test]
    fn the_runs_cover_the_stream_in_order() {
        let quads = vec![
            pane(0.0, 300.0),
            opaque(10.0, 20.0),
            pane(20.0, 60.0),
            pane(20.0, 60.0),
            opaque(0.0, 1.0),
        ];
        let batches = glass_batches(&quads, MAX_GLASS_BATCHES);

        let mut previous = 0;
        for batch in &batches {
            assert!(batch.end > previous, "a run may not be empty or go back");
            previous = batch.end;
        }
        assert_eq!(previous, quads.len());
    }

    /// An empty frame still describes itself, rather than leaving the renderer
    /// to guess whether there is a run to draw.
    #[test]
    fn a_frame_with_nothing_in_it_is_one_empty_run() {
        assert_eq!(
            glass_batches(&[], MAX_GLASS_BATCHES),
            vec![Batch {
                end: 0,
                reads: false
            }]
        );
    }
}
