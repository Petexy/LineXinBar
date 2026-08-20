//! wgpu renderer.
//!
//! Two pipelines draw the interface:
//!
//! * a full-screen pass that draws the animated lattice backdrop in a shader, and
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

use std::collections::{HashMap, HashSet};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

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
    /// in the layout: a modal panel is thick and heavily frosted, while the
    /// disc under a chosen icon is just as thick and nearly clear.
    ///
    /// It does need some thickness to be about, though. A pane with none
    /// transmits nothing, and there is nothing to scatter.
    pub frost: f32,
    /// How strongly this pane takes the light: 0 leaves a flat shape, 1 is
    /// glass under the one lamp the whole shell is lit by.
    ///
    /// The rim, sheen and shadow along the underside are not painted bands;
    /// they are what a slab of this shape does with the shared lamp, and the
    /// shader works them out from the surface. This number only governs how
    /// strongly that calculated reflection is shown.
    pub gloss: f32,
    /// How gently the broad face bows into the key light.
    ///
    /// Zero leaves the face optically flat, which is right for compact
    /// controls. A large sheet can opt into a shallow curve so its reflection
    /// changes across the whole surface instead of existing only in the rim.
    /// This is reflection geometry, not a painted gradient: the same lamp and
    /// environment still decide where the sheen falls.
    pub face_curve: f32,
    /// How much of the colour is taken out of what this quad draws: 0 leaves
    /// the picture as it was made, 1 is grey.
    ///
    /// The one thing in the shell it is used for is a game that is not on this
    /// disk, whose cover is drawn colourless the way a shop draws stock it
    /// does not have. It is here rather than in the picture because the answer
    /// changes while the picture does not — a download finishing has to give a
    /// cover its colour back without the atlas being touched — and because the
    /// same cover is a single copy shared by every display.
    pub drain: f32,
    /// Which part of what this quad samples is actually drawn, as fractions of
    /// the whole: left, top, right, bottom.
    ///
    /// [`WHOLE`] for everything the shell draws but one thing — a photograph in
    /// the round hole a file explorer's row gives it. A picture *fitted* into a
    /// circle is a picture with glass above and below it, and one stretched to
    /// fill the circle is a picture of somebody standing in a funhouse mirror;
    /// the only honest answer is to show the middle of it. See
    /// [`crate::ui::round_crop`], which decides how much middle.
    pub crop: [f32; 4],
    /// The pane's own opacity, multiplied into everything above.
    ///
    /// Separate from the alpha in `color`, which on a glass pane means how
    /// strongly it *stains* the wallpaper rather than how solid it is — a
    /// pane can be deeply tinted and still fading out.
    pub fade: f32,
    /// A rectangle the pane is cut to, in the same pixels as `x` and `y`, or
    /// `None` for one nothing is cutting.
    ///
    /// The counterpart of [`Text::clip`], and it exists for the same reason:
    /// what a *display* cuts has to stay cut when the scene is shrunk into the
    /// guide's start card, which has no edges of its own to hide an overhang
    /// against. Cutting rather than dropping is the whole point — the category
    /// icon a path has carried half off the left of the screen is half of an
    /// icon there and must be half of one in the card, not missing from it.
    ///
    /// It cuts pixels and leaves the shape alone: the rounded corners, the
    /// bevel and the light on it are all still those of the whole pane, so a
    /// cut one reads as a pane running past an edge rather than as a smaller
    /// pane with a straight side.
    pub clip: Option<[f32; 4]>,
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
            face_curve: 0.0,
            drain: 0.0,
            // The whole of whatever this quad samples, which is what every
            // drawing in the shell but a round preview wants.
            crop: WHOLE,
            // Written out because this is the one field whose zero is wrong:
            // every `..Quad::default()` in the shell would draw nothing.
            fade: 1.0,
            clip: None,
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
        // Not a glyph, which has a depth and is shaded rather than traced
        // through. That is a decision and not an oversight: a panel draws a
        // dozen marks over surfaces drawn moments earlier in the same run, and
        // handing each one a snapshot of its own would spend the whole of
        // [`MAX_GLASS_BATCHES`] on icons. See `glyph_material` in shaders.wgsl,
        // which is why one is lit instead of transparent.
        !self.glyph_material() && self.radius > 0.0 && self.border <= 0.0 && self.thickness > 0.0
    }

    /// Whether this quad's cell holds the *shape* of one of the shell's own
    /// glyphs rather than a picture of one, and so is to be shaded as a bead of
    /// water standing on whatever is below it.
    ///
    /// The same test `fs_quad` makes, written once on this side so the two
    /// cannot drift: a square-cornered quad with a depth in it is nothing else
    /// the shell draws.
    pub fn glyph_material(&self) -> bool {
        self.radius <= 0.0 && self.thickness > 0.0
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
/// stopped frosting. The shell's busiest screen is the power dialog over the
/// guide over the bar: the bar's lit discs sit on blooms, the chips sit on the
/// sidebar, the dialog sits on its scrim and its rows sit on the dialog. Keep
/// generous headroom over that composition, because the way a shortage is
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
/// makes the selected entry unmistakable — the lattice highlight.
pub const GLOW_SLOT: u32 = 1;

/// Atlas cells taken by the procedural sprites above; icons start after them.
const RESERVED_SLOTS: u32 = 2;

/// Cells kept back for icons that are not known until the session is running.
///
/// Everything else in the atlas is decided before the first frame: the shell's
/// own glyphs, and one cell per icon named by anything in the launcher's
/// catalogue. That covers every picture the shell draws *of its own accord*,
/// and none of the ones a program hands it — an announcement carries whatever
/// icon its sender chose, which may be a theme name no installed application
/// uses, or a file somewhere on the disk.
///
/// Two dozen, which is far more than a corner of the screen ever shows at
/// once and small enough to cost nothing: at a cell each that is under a
/// megabyte and a half of a texture that already runs to tens.
const LATE_CELLS: u32 = 24;

/// How many cells a thumbnail spans, per side.
///
/// [`crate::thumbs::SIZE`] over [`CELL`]: a thumbnail is a picture rather than
/// a mark, drawn on a card several times the width of an icon, and one cell of
/// it would be visibly soft there.
const THUMB_CELLS: u32 = crate::thumbs::SIZE.div_ceil(CELL);

/// How many thumbnails the atlas holds at once.
///
/// Not a cache size: the shell asks for the rows around the cursor and drops
/// the rest every frame, so this only has to cover what one screen can show
/// with room to move. Eight megabytes of the atlas.
///
/// Two kinds of picture share the band — a frame of one of the user's own
/// films and a Steam cover — because they are the same thing to draw and the
/// same thing to throw away. What sets the number is therefore what *both*
/// cursors can be looking at: a handful of rows either side of each display's
/// cursor, twice over, on a machine with two screens.
const THUMB_BLOCKS: u32 = 32;

/// How many cells a game's logo spans, per side.
///
/// [`crate::art::LOGO_SIZE`] over [`CELL`], which is five — a band of its own
/// rather than a corner of the thumbnails' because a logo is two and a half
/// times the edge of one. It could not be squeezed into a thumbnail block
/// without being scaled down first, and the whole reason it has a band is that
/// it is the one picture in this shell drawn at a third of a display across:
/// what a row's card can hide, a splash cannot.
const LOGO_CELLS: u32 = crate::art::LOGO_SIZE.div_ceil(CELL);

/// How many logos the atlas holds at once.
///
/// The same number as [`HERO_LAYERS`] and for the same reason: a logo is
/// wanted exactly where a hero is — the game each display's cursor is standing
/// on — so what has to fit is one per display with the pair most machines have
/// and a little room over. Four blocks is six megabytes of a texture whose
/// thumbnail band is already eight.
const LOGO_BLOCKS: u32 = 4;

/// How many pictures may stand behind a display at once.
///
/// Two per display — the one going and the one arriving — and every display
/// has its own cursor and therefore its own game. Four covers the pair of
/// screens most machines have; past that a display keeps the shell's own
/// wallpaper, which is what every display had before this existed.
const HERO_LAYERS: u32 = 4;

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

/// The characters the start screen's corner is written in, and the cells they
/// are measured into.
///
/// The corner is the one place in this shell where *type* is drawn as the same
/// material as the marks beside it — a bead of water, lit by the one lamp above
/// the drawing — rather than as flat coverage through the text pipeline. It can
/// be, because a clock is thirteen characters and not a language: each is cut
/// out of the bundled face once at startup, measured into a signed distance
/// field exactly as a glyph's shape is (see [`crate::icons::distance_field`]),
/// and drawn as one quad per letter.
///
/// The rule that keeps that from becoming a text renderer is that the set is
/// *closed*: a fixed list of characters, cut once, because they are marks. A
/// window title is somebody else's alphabet and will never be in one of these.
/// The second such list is [`crate::icons::INDEX_LETTERS`], the headings a long
/// column can be indexed by, and it is a list of its own rather than more rows
/// here — a clock and an index share no character, and one list holding both
/// would be a set whose reason for stopping where it does could not be
/// written down.
///
/// The fourteenth is the per cent sign, and it is here on exactly that
/// argument rather than in spite of it. What it writes is the battery's charge,
/// which stands in this same corner, in this same material, on the same line —
/// see [`crate::ui::corner_percent`]. A number drawn there through the text
/// pipeline would be flat coverage sitting between two beads of water. Adding a
/// character for a second thing the corner says is not the same as opening the
/// set to an alphabet: three digits and a sign is still not a language.
///
/// The space is in the set for its *advance* and has no cell of its own —
/// nothing to measure, and a field with no shape in it would fail the same test
/// an empty glyph does.
const LETTER_SET: [(char, &str); 14] = [
    ('0', "lxb:letter-0"),
    ('1', "lxb:letter-1"),
    ('2', "lxb:letter-2"),
    ('3', "lxb:letter-3"),
    ('4', "lxb:letter-4"),
    ('5', "lxb:letter-5"),
    ('6', "lxb:letter-6"),
    ('7', "lxb:letter-7"),
    ('8', "lxb:letter-8"),
    ('9', "lxb:letter-9"),
    (':', "lxb:letter-colon"),
    ('/', "lxb:letter-slash"),
    ('%', "lxb:letter-percent"),
    (' ', ""),
];

/// The square of the text a letter's cell covers, as a multiple of the type's
/// size, and where the middle of that square sits above the baseline.
///
/// One square for every character rather than a tight box each, and that is the
/// whole of what keeps the run looking like one object: the shader's bevel is a
/// fixed fraction of the *quad*, so a colon in a box its own size would be
/// modelled twice as deeply as the digits either side of it. The square is
/// centred on each character's own advance, so the letters keep the spacing the
/// face gives them.
///
/// An em covers every character in the set with margin to spare for the
/// shadow — the tallest of them is the slash, which reaches from a little below
/// the baseline to a little under the cap. The numbers are held to that by
/// `every_letter_of_the_clock_is_a_shape_in_its_cell`.
pub const LETTER_BOX: f32 = 1.0;
pub const LETTER_MIDDLE: f32 = 0.35;

/// The same two numbers for an index's headings, which are capitals and want
/// their own.
///
/// Wider than an em, and centred on the *cap* rather than on the run. Every
/// heading but two is a plain capital between the baseline and the cap line, so
/// that is what the eye centres a row's mark on — but `Q` hangs a tail below the
/// baseline and `W` is nearly an em across, and a mark that ran out to its cell
/// edge would have its shadow end in a straight cut (see
/// [`crate::icons::distance_field`], and the margin the icons tests hold every
/// mark to). The extra fifteenth is what those two need and the rest of the
/// alphabet spends on air.
const INDEX_BOX: f32 = 1.15;
const INDEX_MIDDLE: f32 = 0.355;

/// And the box the *row* an index hangs under is cut in, which holds three
/// characters instead of one — see [`crate::icons::INDEX_MARK`].
///
/// Nearly two ems, because that is what "A-Z" is wide, and the cell is square:
/// a run has to fit across it with the same margin a single heading keeps, so
/// the type is cut smaller and the mark comes out shorter than the letters it
/// stands over. That is the right way round. The row is the way *in* to the
/// alphabet and the headings are the alphabet, so a mark that stood as tall as
/// them would read as one more of them.
///
/// Centred on the cap line like the headings, since it is capitals; the hyphen
/// finds its own height between them.
const INDEX_MARK_BOX: f32 = 1.9;

/// One of the corner's characters, ready to be drawn.
///
/// The cell it was measured into — `None` for the space, which has an advance
/// and nothing to draw — and how far the pen moves after it, as a multiple of
/// the type's size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Letter {
    pub cell: Option<u32>,
    pub advance: f32,
}

/// The size a set's letters are cut at, given how many ems of type its cell
/// covers.
///
/// Large enough that the supersampled grid the distance transform runs on is
/// the letter's own resolution rather than a guess at it: the cell is measured
/// at [`crate::icons::SDF_SUPERSAMPLE`] times [`CELL`], and the box is exactly
/// that many pixels across whatever share of an em it is.
fn letter_field_size(box_ems: f32) -> f32 {
    (CELL * crate::icons::SDF_SUPERSAMPLE) as f32 / box_ems
}

/// Which cell one of the corner's characters is filed under, if it has one.
fn letter_name(letter: char) -> Option<&'static str> {
    LETTER_SET
        .iter()
        .find(|(c, _)| *c == letter)
        .map(|(_, name)| *name)
        .filter(|name| !name.is_empty())
}

/// The shell's own two faces and nothing else.
///
/// For measuring rather than for drawing: what is wanted here is *this* type,
/// and a system fallback chain would answer with whatever the machine has. The
/// drawing side keeps the fallbacks — see [`Gpu::new`] — because a window title
/// may be in an alphabet Roboto has never heard of.
fn shell_faces() -> FontSystem {
    let mut db = glyphon::fontdb::Database::new();
    for face in [UI_FONT_REGULAR, UI_FONT_BOLD] {
        db.load_font_data(face.to_vec());
    }
    FontSystem::new_with_locale_and_db("en-US".to_string(), db)
}

/// Shape a run of the shell's own type on its own line and answer with the
/// glyphs it came out as.
///
/// `None` if the face has no glyph for any of it, which for these sets would
/// mean the bundled font had been replaced by something that is not Roboto —
/// and it is the whole run that fails, because half a mark is worse than none.
///
/// One character is the ordinary case and a short run is the exception, rather
/// than two ways of cutting a cell: a mark that is three letters is still one
/// mark, laid out by the same shaper that lays out the one-letter ones. What
/// the caller does with the glyphs is the same either way — see
/// [`letter_fields`].
fn shaped_run(
    font_system: &mut FontSystem,
    content: &str,
    size: f32,
) -> Option<Vec<glyphon::cosmic_text::LayoutGlyph>> {
    let mut buffer = TextBuffer::new(font_system, Metrics::new(size, size));
    buffer.set_size(None, None);
    let attrs = Attrs::new()
        .family(Family::Name(UI_FONT))
        .weight(Weight::NORMAL);
    buffer.set_text(content, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(font_system, false);
    let glyphs = buffer.layout_runs().next()?.glyphs.to_vec();
    if glyphs.is_empty() || glyphs.iter().any(|glyph| glyph.glyph_id == 0) {
        return None;
    }
    Some(glyphs)
}

/// Shape one character on its own and answer with the glyph it came out as.
fn shaped_letter(
    font_system: &mut FontSystem,
    letter: char,
    size: f32,
) -> Option<glyphon::cosmic_text::LayoutGlyph> {
    shaped_run(font_system, &letter.to_string(), size)?
        .into_iter()
        .next()
}

/// Cut the shell's own closed sets of type out of the bundled face and measure
/// each mark into the cell the quad shader shades a shape out of.
///
/// Done on the thread that decodes the built-in glyphs and handed to the atlas
/// with them — see `load_builtin_icons` — because it is the same work: a
/// coverage grid at four times the cell, then one exact distance transform. On
/// the main thread it would be a hundred milliseconds of the first frame.
pub fn letter_fields() -> Vec<(String, Icon)> {
    let mut font_system = shell_faces();
    let mut swash = SwashCache::new();
    let fine = CELL * crate::icons::SDF_SUPERSAMPLE;
    let mut out = Vec::new();

    // Every closed set, cut the same way and into the same kind of cell: the
    // corner's clock, the headings an index of a long column is written in, and
    // the mark on the row that index hangs under. Each carries the box it is
    // centred in, because a row of digits, a capital on its own and a run of
    // three are not centred on the same line — see [`LETTER_SET`], [`INDEX_BOX`]
    // and [`INDEX_MARK_BOX`].
    let cutting = LETTER_SET
        .iter()
        .map(|(letter, name)| (letter.to_string(), *name, LETTER_BOX, LETTER_MIDDLE))
        .chain(
            crate::icons::INDEX_LETTERS
                .iter()
                .map(|(letter, name)| (letter.to_string(), *name, INDEX_BOX, INDEX_MIDDLE)),
        )
        .chain(std::iter::once((
            "A-Z".to_string(),
            crate::icons::INDEX_MARK,
            INDEX_MARK_BOX,
            INDEX_MIDDLE,
        )));
    for (content, name, box_ems, middle) in cutting {
        if name.is_empty() {
            continue;
        }
        let field_size = letter_field_size(box_ems);
        let Some(glyphs) = shaped_run(&mut font_system, &content, field_size) else {
            tracing::warn!(%content, "the bundled face has no such characters");
            continue;
        };

        // Where the run's square sits in the same pixels the masks are in: the
        // pen starts at zero, the baseline is at zero, and up is negative. The
        // whole run is centred across the box, so a mark of three characters
        // stands where a mark of one does.
        let run_width: f32 = glyphs.iter().map(|glyph| glyph.w).sum();
        let box_side = box_ems * field_size;
        let left = run_width * 0.5 - box_side * 0.5;
        let top = -(middle * field_size) - box_side * 0.5;

        let mut inside = vec![false; (fine * fine) as usize];
        let mut drawn = false;
        for glyph in &glyphs {
            // The pen at the origin, so each mask's placement is measured from
            // the run's own baseline and its own start and nothing else.
            let physical = glyph.physical((0.0, 0.0), 1.0);
            let Some(image) = swash.get_image_uncached(&mut font_system, physical.cache_key) else {
                tracing::warn!(%content, "the face would not rasterise a character");
                continue;
            };
            if image.content != glyphon::cosmic_text::SwashContent::Mask {
                tracing::warn!(%content, "a character came back as something other than coverage");
                continue;
            }
            for row in 0..image.placement.height {
                for column in 0..image.placement.width {
                    // Coverage of a half or more is the letter, which is where
                    // the distance field's zero belongs: the transform measures
                    // a shape, and a shape's edge is where it covers half a
                    // pixel.
                    let coverage = image.data[(row * image.placement.width + column) as usize];
                    if coverage < 128 {
                        continue;
                    }
                    let x = physical.x + image.placement.left + column as i32 - left.round() as i32;
                    let y = physical.y - image.placement.top + row as i32 - top.round() as i32;
                    if x < 0 || y < 0 || x >= fine as i32 || y >= fine as i32 {
                        // A mark that does not fit its own square would be
                        // drawn with a straight cut down it. The test holds the
                        // box big enough; this is what stops a bad one
                        // corrupting a neighbouring cell instead of being
                        // visible.
                        tracing::warn!(%content, "a character reaches outside its cell");
                        continue;
                    }
                    inside[(y as u32 * fine + x as u32) as usize] = true;
                    drawn = true;
                }
            }
        }
        if !drawn {
            tracing::warn!(%content, "a mark came out with nothing in it");
            continue;
        }

        match crate::icons::distance_field(&inside, fine, CELL) {
            Some(icon) => out.push((name.to_string(), icon)),
            None => tracing::warn!(%content, "a mark would not measure"),
        }
    }
    out
}

/// How far the pen moves after each of the corner's characters, in ems.
///
/// Measured from the bundled faces alone, like the cells themselves: a machine
/// with its own copy of Roboto installed would otherwise be able to answer this
/// with metrics the letters were not cut to.
///
/// The corner adds these up to lay its run out, which is exactly what shaping
/// would answer — there is no kerning between any pair in this set, and
/// `the_corners_run_is_as_wide_as_the_same_letters_shaped` holds it to that.
fn letter_advances() -> HashMap<char, f32> {
    let mut font_system = shell_faces();
    let mut out = HashMap::new();
    for (letter, _) in LETTER_SET {
        match shaped_letter(&mut font_system, letter, LETTER_ADVANCE_SIZE) {
            Some(glyph) => {
                out.insert(letter, glyph.w / LETTER_ADVANCE_SIZE);
            }
            // A face with no such character is a face this shell was not built
            // with. The corner then draws no clock at all rather than a run
            // with a hole in it — see `ui::build`.
            None => tracing::warn!(%letter, "no advance for one of the clock's characters"),
        }
    }
    out
}

/// The size the advances are measured at: large enough that the sixteenths of a
/// pixel a face quantises to are noise against it, and the answer is a ratio
/// either way.
const LETTER_ADVANCE_SIZE: f32 = 1000.0;

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
    /// Part of the key because it changes the shaping: the same words in the
    /// same box are one line with an ellipsis or three without, depending on
    /// how many they are allowed. A row growing to be read would otherwise
    /// keep the cut-off shaping it had while it was one of a column.
    lines: u8,
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
            lines: text.lines.max(1),
        }
    }
}

/// How far a halo reaches out from its letters, as a share of the run's size.
///
/// A share rather than a number of pixels because it has to hold at every
/// scale the shell draws at: a ring a fixed two pixels wide is a heavy outline
/// on a small label and invisible under a heading.
///
/// It was a tenth, on the reasoning that the ring is there to give the letters
/// an edge to sit against and anything wider would read as a sticker. What that
/// missed is that an edge is only enough when the thing behind it is *quiet*.
/// Over the bar's white category icons, coming up through the glass, a stroke's
/// width of shade is not a background — the letter still sits in a bright
/// field, and it is the field that has to come down. A wider shadow is a
/// darker patch of what the eye reads the word against.
const HALO_RING: f32 = 0.24;

/// How far apart the copies may fall along a ring, in pixels.
///
/// The number that makes a radius safe to raise. Copies are spread evenly
/// round a circle, so the further out a ring is, the further apart they land at
/// the same count — and once they are further apart than a letter's stroke is
/// wide, they stop overlapping and the shadow stops being a shadow. It becomes
/// a row of little copies of the word, which at a tenth of the size was too
/// small to notice and at a quarter would not be.
///
/// So the count follows the radius instead of being fixed, and this is what it
/// follows: a shade under a pixel and a half apart is one nobody can pick the
/// dots out of.
const HALO_SPACING: f32 = 1.3;

/// The fewest copies a ring is ever made of, however tight it is.
const HALO_MIN_STEPS: usize = 8;

/// The rings, as (how far out, how dark each copy is) — from the one that does
/// the work out to the faint one that takes the edge off.
///
/// More than one ring is the whole difference between a soft shadow and an
/// outline. A single ring has a hard outer edge at exactly its radius, because
/// every copy stops there together; the ones further out and fainter leave the
/// darkness falling off instead of ending. Three now rather than two, because
/// the gap between two rings is a share of the reach and the reach has more
/// than doubled — the same two would leave a visible step in the falloff.
///
/// The copies overlap, and that is the point — near the letters many of them
/// cover the same pixel and the shade builds up, further out only one or two
/// do. What comes out is a gradient nobody had to draw.
///
/// The weights are per copy and lower than they were, because a wider ring is
/// made of more of them: what darkens a pixel is how many copies land on it,
/// so holding the per-copy figure while the count grows with the radius would
/// have made the shadow blacker every time it was widened.
///
/// The innermost is cut the hardest of the three, and that is deliberate. What
/// makes a shadow read as *hard* is not how far it reaches but how sharply it
/// starts: a dark core hugging the letters is an outline with a blur around
/// it, however soft the outside is. Taking the core down and leaving the
/// spread nearly alone is what turns the same reach from a stamp into a
/// shadow, and it costs almost nothing in legibility — the letter's own edge
/// was never the part doing the work once the reach grew.
const HALO_RINGS: [(f32, f32); 3] = [(0.34, 0.20), (0.67, 0.13), (1.0, 0.075)];

/// Where the copies that make up a run's halo go, and how dark each one is:
/// `(dx, dy, opacity)` in the run's own pixels.
///
/// The copies are of the run itself, drawn from the shaping it already has, so
/// the shade is the shape of the letters and not of a box around them. Nothing
/// here knows what the words are.
///
/// `halo` is the finished strength and the only thing that decides it. It used
/// to be multiplied by the run's own opacity, on the reasoning that a bubble
/// flying off the display should take its ring with it — true, but it made a
/// run's ring weaker the softer the run's *colour* was, and a soft colour is
/// the exact case that needs the most help reading. The second line of a
/// notification is grey at seven tenths and was getting a seven-tenths ring
/// where it wanted a stronger one than the white line above it.
///
/// So fading is the caller's to do, and a caller with something that fades has
/// to fade this too. Nothing is drawn at zero, which is what makes that safe.
fn halo_copies(size: f32, halo: f32) -> Vec<(f32, f32, f32)> {
    if halo <= 0.0 {
        return Vec::new();
    }
    let reach = size * HALO_RING;
    let mut copies = Vec::new();
    for (ring, (spread, weight)) in HALO_RINGS.iter().enumerate() {
        let radius = reach * spread;
        // As many as it takes to keep them HALO_SPACING apart at this radius,
        // never fewer than HALO_MIN_STEPS. This is what lets the reach be
        // raised without the shadow coming apart into dots.
        let steps =
            ((std::f32::consts::TAU * radius / HALO_SPACING).ceil() as usize).max(HALO_MIN_STEPS);
        let turn = std::f32::consts::TAU / steps as f32;
        // Each ring started part of a step round from the one inside it, so
        // that copies at different radii do not line up into spokes.
        let lead = turn * ring as f32 / HALO_RINGS.len() as f32;
        for step in 0..steps {
            let angle = turn * step as f32 + lead;
            copies.push((
                radius * angle.cos(),
                radius * angle.sin(),
                (halo * weight).clamp(0.0, 1.0),
            ));
        }
    }
    copies
}

/// A run of text to draw.
///
/// Cloneable because a run a panel stands across is drawn twice, once for the
/// part either side of it — see [`crate::ui::Scene::hide_text_behind`]. The two
/// share a shaping: it is keyed on everything but the clip.
#[derive(Clone)]
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
    /// A rectangle the run is cut to, in the same pixels as `x` and `y`, or
    /// `None` for a run nothing is standing over.
    ///
    /// Deliberately separate from `max_width`, which is the *box the run is laid
    /// out in*: narrowing that re-shapes the text and puts an ellipsis at the
    /// end, which is the right answer for a title too long for its column and
    /// the wrong one for a label half-covered by a panel. This cuts the pixels
    /// and leaves the layout alone, so a run being covered says nothing about
    /// where its words are — it is exactly what a thing in front of it does.
    ///
    /// See [`crate::ui::Scene::hide_text_behind`], which is where every one of
    /// these comes from.
    pub clip: Option<[f32; 4]>,
    /// How strongly the letters are ringed in shade, as an opacity.
    ///
    /// Zero — no ring at all — for very nearly every run the shell draws, and
    /// deliberately so: a label on a panel the shell chose the colour of has
    /// its contrast decided already, and outlining it would only make it look
    /// stamped on.
    ///
    /// It is for the runs standing on something nobody chose. A pane of glass
    /// shows what is behind it, so writing on one is legible or not depending
    /// on what happens to be underneath — over the bar's white category icons
    /// a line of grey text has nothing to be grey *against*. The ring gives
    /// each letter its own background, the exact shape of the letter, which is
    /// the one way to buy contrast without taking a whole rectangle of the
    /// screen darker.
    ///
    /// See [`HALO_RING`] for how it is drawn.
    pub halo: f32,
    /// How many lines this run may take before it is cut with an ellipsis.
    ///
    /// One for every run in the shell but the one that has grown to be read —
    /// see [`crate::ui::context_row_growth`]. A label is a label: it names one
    /// thing, it belongs on one line, and a column of labels that each wrapped
    /// to their own height would be a list nobody can scan.
    ///
    /// The exception is a row somebody has *stopped on*. That row is no longer
    /// one of a column being scanned, it is the one thing being read, and a
    /// sentence cut off with an ellipsis is a sentence the user opened the
    /// panel to see the end of.
    pub lines: u8,
}

impl Default for Text {
    fn default() -> Self {
        Self {
            content: String::new(),
            x: 0.0,
            y: 0.0,
            size: 16.0,
            color: [1.0; 4],
            bold: false,
            max_width: f32::MAX,
            align: TextAlign::Left,
            clip: None,
            halo: 0.0,
            lines: 1,
        }
    }
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
    /// the four above because they are full.
    corner: f32,
    /// Broad-face reflection curvature, separate so compact controls keep the
    /// perfectly level face they have always had.
    face_curve: f32,
    /// How much of the colour is taken out of what is sampled.
    drain: f32,
    /// The rectangle this pane is cut to, as its two corners rather than as a
    /// size: that is the comparison the shader makes against each pixel, and
    /// the conversion belongs here rather than once per fragment. A pane with
    /// nothing cutting it is given a box larger than any display.
    cut: [f32; 4],
}

/// All of what a quad samples: see [`Quad::crop`].
pub const WHOLE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// The part of an atlas rectangle a quad's [`crop`](Quad::crop) keeps.
///
/// In the rectangle's own units rather than the atlas's, so a crop says the
/// same thing about a picture wherever in the atlas that picture landed and
/// however much of its block it covers.
fn cropped([u0, v0, u1, v1]: [f32; 4], [x0, y0, x1, y1]: [f32; 4]) -> [f32; 4] {
    let (w, h) = (u1 - u0, v1 - v0);
    [u0 + w * x0, v0 + h * y0, u0 + w * x1, v0 + h * y1]
}

/// The `cut` of a pane nothing is cutting: far enough out that no pixel of any
/// display can fall outside it.
const UNCUT: [f32; 4] = [-1.0e9, -1.0e9, 1.0e9, 1.0e9];

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
    /// The picture standing behind everything: the layer being left, the layer
    /// being arrived at, and how much of each is showing. See [`Hero`].
    hero: [f32; 4],
    /// Which material each half of the shell is drawn in: 0 for its own and 1
    /// for the plain one a slow machine asks for, under Settings > Appearance >
    /// Theme. `x` is the wallpaper — the band of water against the glass-silk
    /// ribbons — and `y` is every mark the shell draws, beaded out of its own
    /// shape against the flat shape itself. Two numbers rather than one because
    /// the two are separate settings.
    ///
    /// `x` has a third value, 2, which is not a material: the user's own picture
    /// or film, drawn instead of the scene. It is written only while there is
    /// really a picture in [`Gpu::paper`] — see [`Gpu::wallpaper_flag`] — so the
    /// shader never has to ask whether the texture it is about to read holds
    /// anything. `z` is that picture's own shape, width over height, which is
    /// what the crop to the display is worked out from; it is nought when there
    /// is none. `w` is spare, and a uniform block is laid out in sixteen-byte
    /// lots, so it costs nothing to leave it there.
    style: [f32; 4],
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

/// How many rungs the user's own wallpaper carries, counting the sharp copy.
///
/// The same five as the backdrop and the scenery, and for the same reason: the
/// wallpaper is asked for softened — behind the guide, and through every frosted
/// pane in the shell — and an analytic scene answers that by drawing itself
/// dimmer and wider, where a photograph can only answer it by having smaller
/// copies to be read from. A file smaller than 32 pixels down its shorter edge
/// gets fewer, because there is nowhere to halve to.
const PAPER_MIPS: u32 = 5;

/// What the user's own wallpaper is held in.
///
/// Not the surface's format, which is the display's business and is BGRA on
/// nearly every machine. This one is what a decoded frame *is* — see
/// [`crate::paper`], where `libswscale` is asked for `RGBA` — and holding it in
/// anything else would mean swapping two bytes of every pixel of every frame of
/// a film on the way to the GPU. sRGB, because a picture is authored in sRGB and
/// the sampler is what converts it to the linear light the shell mixes in.
const PAPER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// The user's own wallpaper on the GPU: one picture, or the newest frame of one
/// film, and the chain of halvings frost reads.
///
/// The halvings are rendered here rather than made on the CPU as the scenery's
/// are, and the difference is the film: a still picture's chain is built once
/// and a film's is built for every frame that reaches the screen, which is sixty
/// times a second. Four passes over ever-smaller textures is a few tenths of a
/// millisecond on the GPU that is about to draw the frame anyway; the same
/// arithmetic on the thread that draws would be milliseconds taken out of it.
struct Paper {
    texture: wgpu::Texture,
    /// One view per rung. The first is what a frame is written into; the rest
    /// are rendered from the rung above.
    rungs: Vec<wgpu::TextureView>,
    /// One bind group per rung, for reading it while the next is drawn.
    sources: Vec<wgpu::BindGroup>,
    width: u32,
    height: u32,
}

impl Paper {
    /// A texture of this size, with as many halvings as it has room for, and
    /// everything needed to render them.
    fn new(
        device: &wgpu::Device,
        sample_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
        width: u32,
        height: u32,
    ) -> Paper {
        // As many halvings as the picture has room for, so a small drawing does
        // not ask for a rung no pixels wide.
        let levels = PAPER_MIPS.min(width.min(height).ilog2().max(1));
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("custom wallpaper"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PAPER_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let rungs: Vec<wgpu::TextureView> = (0..levels)
            .map(|level| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("custom wallpaper rung"),
                    base_mip_level: level,
                    mip_level_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
        let sources = rungs
            .iter()
            .map(|view| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("custom wallpaper rung"),
                    layout: sample_layout,
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
            })
            .collect();
        Paper {
            texture,
            rungs,
            sources,
            width,
            height,
        }
    }
}

/// Bind the pictures behind the shell: Steam's, the user's own file thumbnails
/// blown up to a display, and the wallpaper somebody chose.
///
/// One function because the group is built twice — once with nothing in the
/// wallpaper's slot, and again each time a wallpaper of a new *size* arrives,
/// which is a new texture and therefore a new view to point at. A frame of a
/// film that is the same size as the last one changes nothing here.
fn scenery_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    scenery: &wgpu::Texture,
    sampler: &wgpu::Sampler,
    paper: Option<&wgpu::Texture>,
    blank: &wgpu::Texture,
) -> wgpu::BindGroup {
    let scenery_view = scenery.create_view(&wgpu::TextureViewDescriptor {
        label: Some("scenery"),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        ..Default::default()
    });
    let paper_view = paper
        .unwrap_or(blank)
        .create_view(&wgpu::TextureViewDescriptor {
            label: Some("custom wallpaper"),
            ..Default::default()
        });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("scenery"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&scenery_view),
            },
            // The frame sampler, because it is the one that clamps at the
            // edges and reads down the blur chain — which is exactly what
            // a picture cropped to a display and softened behind the guide
            // needs. Both textures here want precisely that, so they share it.
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::TextureView(&paper_view),
            },
        ],
    })
}

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

/// The picture standing behind the whole shell on one display, as it is this
/// frame.
///
/// Steam's own picture of the game under the cursor, crossfading to the next
/// one as the cursor moves. Two layers rather than one because a picture must
/// not blink out to make room for its successor, and because a fade that went
/// through the wallpaper on the way would announce the wallpaper rather than
/// the game.
///
/// Deliberately *not* part of [`Backdrop`], which is one pass on one surface.
/// This is what the wallpaper *is*, and the wallpaper is drawn in two places:
/// the surface below the bar, and inside every pane of glass on the bar, which
/// re-creates what is behind it rather than reading it. Both have to be told
/// the same thing or a pane refracts a wallpaper nobody can see.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Hero {
    /// The layer being faded out of, and how much of it is left showing.
    pub from: Option<u32>,
    pub leaving: f32,
    /// The layer being faded into, and how much of it has arrived.
    pub to: Option<u32>,
    pub arriving: f32,
}

impl Hero {
    /// The four numbers the shader reads. A layer that is not there is given
    /// no strength, so the shader never has to ask whether one exists.
    fn packed(&self) -> [f32; 4] {
        let layer = |which: Option<u32>| which.unwrap_or(0) as f32;
        [
            layer(self.from),
            layer(self.to),
            self.from.map_or(0.0, |_| self.leaving.clamp(0.0, 1.0)),
            self.to.map_or(0.0, |_| self.arriving.clamp(0.0, 1.0)),
        ]
    }
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
    /// And the same for the user's own wallpaper, which is the one texture here
    /// that is not in the surface's format. See [`PAPER_FORMAT`].
    paper_downsample_pipeline: wgpu::RenderPipeline,
    /// The finished frame, from the texture it was built in onto the display.
    blit_pipeline: wgpu::RenderPipeline,
    globals_layout: wgpu::BindGroupLayout,
    atlas_layout: wgpu::BindGroupLayout,
    atlas_sampler: wgpu::Sampler,
    /// One texture and one sampler: what both offscreen passes read, and what
    /// binds the backdrop to the quad pipeline.
    sample_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    atlas_bind_group: wgpu::BindGroup,

    /// Icon name to atlas slot, as decided before the first frame.
    slots: HashMap<String, u32>,
    /// The same for icons that arrived with an announcement — see
    /// [`LATE_CELLS`] — and which cell each is in, in band order.
    ///
    /// Two maps rather than one because they are emptied on different terms:
    /// nothing ever leaves `slots`, and a late cell is taken back the moment
    /// the band is full and something new is asked for. Keeping them apart is
    /// what stops an eviction reaching an application's own icon.
    late_slots: HashMap<String, u32>,
    late_cells: Vec<Option<String>>,
    late_first: u32,
    /// Which cell the next eviction takes. Round the band in order, which for
    /// this is as good as choosing the least recently used and needs nothing
    /// remembered: the band is far larger than the handful of announcements
    /// that can be on screen at once, so by the time it comes round again what
    /// was in a cell is long gone.
    late_next: usize,
    atlas_cells_per_row: u32,
    atlas_cells_per_col: u32,
    /// The atlas itself, kept because thumbnails are written into it while the
    /// session runs — the icons were all decoded before the device existed,
    /// but a picture of a film is made the moment somebody looks at the row.
    atlas_texture: wgpu::Texture,
    /// The band of cells set aside for thumbnails: which file each block is
    /// holding, in block order. `None` is a free block.
    thumb_blocks: Vec<Option<PathBuf>>,
    /// Where that band starts, in cell rows.
    thumb_band: u32,
    /// The slot and texture rectangle of each resident thumbnail. Separate
    /// from [`Self::slots`] because a thumbnail is not square: it uses only
    /// part of its block, and [`Self::uv_for`] has to be told which part.
    thumbs: HashMap<PathBuf, Thumb>,
    /// The band under it, holding game logos, and which game each block is
    /// holding. Filed by app id rather than by path: a logo is asked for by
    /// the game it names, and nothing downstream ever sees the file.
    logo_blocks: Vec<Option<u32>>,
    logo_band: u32,
    logos: HashMap<u32, Thumb>,

    /// The pictures that stand behind a display, one per layer of an array
    /// texture, and what each layer is a picture of. `None` is a free layer.
    ///
    /// A texture of its own rather than a corner of the atlas: this one is a
    /// whole display's worth of picture with a chain of blurred copies under
    /// it, and the atlas is a grid of 128-pixel cells with no mip levels at
    /// all.
    scenery_texture: wgpu::Texture,
    scenery_bind_group: wgpu::BindGroup,
    scenery_layers: Vec<Option<crate::art::Sight>>,
    scenery_layout: wgpu::BindGroupLayout,

    /// The user's own wallpaper: one picture, or the newest frame of one film,
    /// with a chain of ever-smaller copies under it.
    ///
    /// Beside the scenery rather than in it, although both are pictures behind
    /// the shell, because they are different shapes and are asked different
    /// questions. Every layer of the scenery is a hero — 1920 by 620, Valve's
    /// shape, cropped to the middle of the display — and a wallpaper is whatever
    /// shape the file is, held at whatever size it came at. Squeezing one into
    /// the other would either letterbox somebody's photograph or throw two
    /// thirds of it away.
    ///
    /// `None` until the first frame arrives, which is the state a session
    /// spends its first moments in and the state a file that cannot be read
    /// stays in for good. Nothing has to test for it downstream — see
    /// [`Gpu::wallpaper_flag`], which is what the shader is told instead.
    paper: Option<Paper>,
    /// One transparent texel, so the wallpaper's binding has something to point
    /// at on a machine that has not set one. See where it is made.
    blank_paper: wgpu::Texture,

    font_system: FontSystem,
    swash_cache: SwashCache,
    text_atlas: TextAtlas,
    text_cache: Cache,
    /// How far the pen moves after each character the corner's clock is written
    /// in. See [`letter_advances`], and [`Gpu::letter`], which is what the
    /// layout asks.
    letter_advances: HashMap<char, f32>,
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
    pub unsafe fn new_wallpaper(
        display: *mut std::ffi::c_void,
        surface: *mut std::ffi::c_void,
        width: u32,
        height: u32,
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
            label: Some("lxb-desktop"),
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
        let atlas = build_wallpaper_atlas(&device, &queue);
        let atlas_view = atlas
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
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

        // --- the pictures behind the shell --------------------------------
        let scenery_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scenery layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2Array,
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
                // The user's own wallpaper, in the same group as the pictures
                // that stand *in front of* it: one group, because the wallpaper
                // is one function and both passes that draw it need everything
                // that function reads.
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let scenery_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scenery"),
            size: wgpu::Extent3d {
                width: crate::art::HERO_WIDTH,
                height: crate::art::HERO_HEIGHT,
                depth_or_array_layers: HERO_LAYERS,
            },
            mip_level_count: crate::art::HERO_LEVELS,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // Something for the wallpaper's binding to point at before there is a
        // wallpaper — and on every machine that never sets one, which is most of
        // them. A bind group has to be complete whether or not the shader will
        // read it, and one transparent texel is the cheapest complete answer.
        let blank_paper = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("no custom wallpaper"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PAPER_FORMAT,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let scenery_bind_group = scenery_group(
            &device,
            &scenery_layout,
            &scenery_texture,
            &frame_sampler,
            None,
            &blank_paper,
        );

        // --- pipelines ---------------------------------------------------
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lattice shaders"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders.wgsl").into()),
        });

        // The wallpaper is one function, and it samples the picture behind the
        // shell. Both passes that draw it therefore need the scenery bound at
        // the same place; the two groups in between are the quad pass's own
        // and are holes here.
        let background_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("background layout"),
            bind_group_layouts: &[Some(&globals_layout), None, None, Some(&scenery_layout)],
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
        let offscreen_pipeline = |label: &str, entry: &str, format: wgpu::TextureFormat| {
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
        let downsample_pipeline = offscreen_pipeline("downsample", "fs_downsample", format);
        let blit_pipeline = offscreen_pipeline("blit", "fs_blit", format);
        // The same halving again, for the one texture in the shell that is not
        // in the surface's own format: the user's own wallpaper, which arrives
        // as decoded RGBA. A pipeline's colour target has to be the format it
        // will really be drawn into — a display whose surface is BGRA refuses
        // the pass outright — and converting every frame of a film on the way
        // in would be a pass over eight megabytes to save one pipeline.
        let paper_downsample_pipeline =
            offscreen_pipeline("custom wallpaper downsample", "fs_downsample", PAPER_FORMAT);

        let quad_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quad layout"),
            bind_group_layouts: &[
                Some(&globals_layout),
                Some(&atlas_layout),
                Some(&sample_layout),
                Some(&scenery_layout),
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
                        6 => Float32,
                        7 => Float32,
                        8 => Float32x4,
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
                paper_downsample_pipeline,
                blit_pipeline,
                globals_layout,
                atlas_layout,
                atlas_sampler: sampler,
                sample_layout,
                sampler: frame_sampler,
                atlas_bind_group,
                atlas_cells_per_row: atlas.cells_per_row,
                atlas_cells_per_col: atlas.cells_per_col,
                thumb_blocks: vec![None; atlas.thumb_blocks],
                thumb_band: atlas.thumb_band,
                thumbs: HashMap::new(),
                logo_blocks: vec![None; atlas.logo_blocks],
                logo_band: atlas.logo_band,
                logos: HashMap::new(),
                scenery_texture,
                scenery_bind_group,
                scenery_layers: vec![None; HERO_LAYERS as usize],
                scenery_layout,
                paper: None,
                blank_paper,
                atlas_texture: atlas.texture,
                slots: atlas.slots,
                late_slots: HashMap::new(),
                late_cells: vec![None; atlas.late_cells],
                late_first: atlas.late_first,
                late_next: 0,
                font_system,
                swash_cache,
                text_atlas,
                text_cache,
                letter_advances: letter_advances(),
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

    /// What the shell is drawing through, as the adapter names itself.
    ///
    /// The one fact on the System information panel that is not on the disk
    /// somewhere: which of a machine's adapters is in use is a decision this
    /// renderer made when it opened one — see [`Gpu::new`] — and asking the
    /// kernel afterwards would answer with all of them and no way to tell
    /// which. Whole and unedited, driver name and all — a Mesa adapter calls
    /// itself `Some Card (SOMEDRV CHIP)` — because deciding which half of that
    /// a *row* has room for is the panel's business and not the renderer's.
    /// See [`crate::machine::Facts::read`], which is where it is shortened.
    pub fn graphics(&self) -> String {
        self.adapter.get_info().name
    }

    /// Atlas slot for an icon name, if it was loaded.
    pub fn slot(&self, name: &str) -> Option<u32> {
        self.slots
            .get(name)
            .or_else(|| self.late_slots.get(name))
            .copied()
    }

    /// One of the corner's characters: the cell it was measured into and how far
    /// the pen moves after it.
    ///
    /// `None` for anything outside the set the clock is written in — the corner
    /// draws no clock at all rather than a run with a hole in it. The space
    /// answers with an advance and no cell, because it has nothing to draw.
    pub fn letter(&self, letter: char) -> Option<Letter> {
        let advance = *self.letter_advances.get(&letter)?;
        let cell = letter_name(letter).and_then(|name| self.slot(name));
        // A character with a drawing that is not in the atlas is not drawable,
        // and the run must not close up over it: the atlas is still the
        // provisional one on the first frames of a session, and a clock that
        // arrived a letter at a time as cells appeared would be worse than one
        // that arrives whole.
        if cell.is_none() && letter_name(letter).is_some() {
            return None;
        }
        Some(Letter { cell, advance })
    }

    /// Replace the provisional procedural atlas with the completed catalogue.
    ///
    /// This is deliberately allowed only while every runtime band is empty.
    /// The caller holds the start screen at the back of its arrival and polls
    /// no notification/artwork worker before calling it, and these checks keep
    /// a future reorder from silently discarding one of those pictures.
    pub fn replace_icons(&mut self, icons: Vec<(String, Icon)>) -> anyhow::Result<()> {
        if !self.late_slots.is_empty()
            || !self.thumbs.is_empty()
            || !self.logos.is_empty()
            || self.scenery_layers.iter().any(Option::is_some)
        {
            anyhow::bail!("cannot replace an atlas after runtime pictures were added");
        }

        let atlas = build_atlas(&self.device, &self.queue, icons)?;
        let view = atlas
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.atlas_bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("atlas"),
            layout: &self.atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.atlas_sampler),
                },
            ],
        });
        self.atlas_cells_per_row = atlas.cells_per_row;
        self.atlas_cells_per_col = atlas.cells_per_col;
        self.thumb_blocks = vec![None; atlas.thumb_blocks];
        self.thumb_band = atlas.thumb_band;
        self.thumbs.clear();
        self.logo_blocks = vec![None; atlas.logo_blocks];
        self.logo_band = atlas.logo_band;
        self.logos.clear();
        self.atlas_texture = atlas.texture;
        self.slots = atlas.slots;
        self.late_slots.clear();
        self.late_cells = vec![None; atlas.late_cells];
        self.late_first = atlas.late_first;
        self.late_next = 0;
        Ok(())
    }

    /// Put an icon the session was not started knowing about into the atlas,
    /// and answer with the cell it went in.
    ///
    /// For the pictures programs choose for their own announcements: a theme
    /// name no installed application uses, or a file on the disk. Everything
    /// the shell draws of its own accord is in the atlas before the first
    /// frame, and this is the one way in afterwards.
    ///
    /// Writing one cell straight into the texture, the way a thumbnail is
    /// written — the difference is only that a thumbnail takes a whole block
    /// and this takes a single cell, because an icon is square and no bigger
    /// than one.
    ///
    /// Asking twice for the same name is free: the second call finds it and
    /// hands back the cell it is already in.
    pub fn put_icon(&mut self, name: &str, icon: &crate::icons::Icon) -> Option<u32> {
        if name.is_empty() || self.late_cells.is_empty() {
            return None;
        }
        if let Some(slot) = self.slot(name) {
            return Some(slot);
        }

        let cell = self.late_next % self.late_cells.len();
        self.late_next = self.late_next.wrapping_add(1);
        // Whatever was there stops being findable before the pixels change,
        // so a name can never point at another program's picture.
        if let Some(evicted) = self.late_cells[cell].take() {
            self.late_slots.remove(&evicted);
        }

        let slot = self.late_first + cell as u32;
        let col = slot % self.atlas_cells_per_row;
        let row = slot / self.atlas_cells_per_row;

        // The whole cell every time, for the reason `put_thumbnail` writes a
        // whole block: an icon smaller than the cell would otherwise leave a
        // border of the last one showing round it.
        let size = icon.size.min(CELL);
        let mut pixels = vec![0u8; (CELL * CELL * 4) as usize];
        for y in 0..size {
            let src = (y * icon.size * 4) as usize;
            let dst = (y * CELL * 4) as usize;
            let len = (size * 4) as usize;
            if src + len <= icon.rgba.len() && dst + len <= pixels.len() {
                pixels[dst..dst + len].copy_from_slice(&icon.rgba[src..src + len]);
            }
        }

        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.atlas_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: col * CELL,
                    y: row * CELL,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(CELL * 4),
                rows_per_image: Some(CELL),
            },
            wgpu::Extent3d {
                width: CELL,
                height: CELL,
                depth_or_array_layers: 1,
            },
        );

        self.late_cells[cell] = Some(name.to_string());
        self.late_slots.insert(name.to_string(), slot);
        Some(slot)
    }

    /// How many lines `content` would take, laid out `width` pixels wide, up
    /// to `cap`.
    ///
    /// The one thing the layout cannot work out for itself. Everything in
    /// `ui` is arithmetic on rectangles and has no font system in it — which
    /// is what lets nearly all of it be tested without a GPU — so a row that
    /// has to be as tall as its words are long has to ask the only thing that
    /// knows how wide a word is.
    ///
    /// Asked when a panel is built rather than when it is drawn. A label does
    /// not change between frames, and shaping one on every frame of every row
    /// to discover a number that never moves would be paying a rendering cost
    /// for a layout fact.
    pub fn lines_needed(
        &mut self,
        content: &str,
        size: f32,
        bold: bool,
        width: f32,
        cap: usize,
    ) -> usize {
        if content.is_empty() || width <= 0.0 || size <= 0.0 || cap <= 1 {
            return 1;
        }
        let mut buffer = TextBuffer::new(&mut self.font_system, Metrics::new(size, size * 1.25));
        // Room for `cap` lines and no more: the shaping stops there, which is
        // also the answer this is allowed to give.
        buffer.set_size(Some(width), Some(size * 1.25 * cap as f32));
        let attrs = Attrs::new().family(Family::Name(UI_FONT)).weight(if bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        });
        buffer.set_text(content, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer.layout_runs().count().clamp(1, cap)
    }

    /// The thumbnail resident in the atlas for a file, if there is one.
    pub fn thumbnail(&self, path: &Path) -> Option<Thumb> {
        self.thumbs.get(path).copied()
    }

    /// Put a thumbnail into the atlas, taking a free block.
    pub fn put_thumbnail(&mut self, path: &Path, picture: &crate::thumbs::Picture) -> bool {
        let Some(block) = self.thumb_blocks.iter().position(Option::is_none) else {
            // Only reachable if more rows were asked for than the atlas holds;
            // the shell drops what the cursor has left behind before it asks
            // for more.
            tracing::debug!("no free thumbnail block; this one is not drawn");
            return false;
        };
        let cell = Self::band_cell(
            self.atlas_cells_per_row,
            self.thumb_band,
            THUMB_CELLS,
            block,
        );
        let Some(thumb) = self.write_block(THUMB_CELLS, cell, picture) else {
            return false;
        };
        self.thumb_blocks[block] = Some(path.to_path_buf());
        self.thumbs.insert(path.to_path_buf(), thumb);
        true
    }

    /// The logo resident in the atlas for a game, if there is one.
    pub fn logo(&self, app_id: u32) -> Option<Thumb> {
        self.logos.get(&app_id).copied()
    }

    /// Put a game's logo into the atlas, taking a free block of the band that
    /// is big enough to hold one.
    ///
    /// Answers false when there is no room, which leaves the launch splash
    /// showing the game's name — the same thing it shows for a game Valve has
    /// no logo for.
    pub fn put_logo(&mut self, app_id: u32, picture: &crate::thumbs::Picture) -> bool {
        if self.logos.contains_key(&app_id) {
            return false;
        }
        let Some(block) = self.logo_blocks.iter().position(Option::is_none) else {
            tracing::debug!(app_id, "no free logo block; the splash uses the name");
            return false;
        };
        let cell = Self::band_cell(self.atlas_cells_per_row, self.logo_band, LOGO_CELLS, block);
        let Some(logo) = self.write_block(LOGO_CELLS, cell, picture) else {
            return false;
        };
        self.logo_blocks[block] = Some(app_id);
        self.logos.insert(app_id, logo);
        true
    }

    /// Give up every logo block whose game is not in `wanted`.
    ///
    /// The thumbnails' policy again, and the scenery's: what these hold is
    /// what is about to be drawn, and a logo the cursor has left is one
    /// nothing will draw until it is asked for again.
    pub fn retain_logos(&mut self, wanted: &HashSet<u32>) {
        for block in &mut self.logo_blocks {
            if block.is_some_and(|app_id| !wanted.contains(&app_id)) {
                *block = None;
            }
        }
        self.logos.retain(|app_id, _| wanted.contains(app_id));
    }

    /// Write one picture into a block of `cells` cells a side, whose top-left
    /// cell is `(col, row)`, and say what landed there.
    ///
    /// The whole block is written, not just the part the picture covers: a
    /// portrait photograph landing where a wide one was would otherwise leave
    /// two strips of the old one showing beside it, and a full block is one
    /// aligned write rather than a special case. It matters more for a logo
    /// than for anything else here — a wordmark is transparent nearly
    /// everywhere, so whatever was left behind would show *through* it.
    fn write_block(
        &mut self,
        cells: u32,
        (col, row): (u32, u32),
        picture: &crate::thumbs::Picture,
    ) -> Option<Thumb> {
        let edge = cells * CELL;
        if picture.width == 0 || picture.height == 0 {
            return None;
        }

        let width = picture.width.min(edge);
        let height = picture.height.min(edge);
        let mut block = vec![0u8; (edge * edge * 4) as usize];
        for y in 0..height {
            let src = (y * picture.width * 4) as usize;
            let dst = (y * edge * 4) as usize;
            let len = (width * 4) as usize;
            if src + len <= picture.rgba.len() && dst + len <= block.len() {
                block[dst..dst + len].copy_from_slice(&picture.rgba[src..src + len]);
            }
        }

        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.atlas_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: col * CELL,
                    y: row * CELL,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            &block,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(edge * 4),
                rows_per_image: Some(edge),
            },
            wgpu::Extent3d {
                width: edge,
                height: edge,
                depth_or_array_layers: 1,
            },
        );

        Some(Thumb {
            slot: row * self.atlas_cells_per_row + col,
            aspect: width as f32 / height as f32,
            // What was written, not what would have been written by a picture
            // that filled the block: see [`Thumb::covers`].
            covers: Self::block_coverage(width, height, cells),
        })
    }

    /// Give up every thumbnail block whose file is not in `wanted`.
    ///
    /// This is the whole of the eviction policy, and it is deliberately not a
    /// cache: what the atlas holds is what is on screen. A row the cursor has
    /// scrolled away from is a picture nothing is drawing, and the disk cache
    /// underneath means getting it back costs a read rather than a decode.
    pub fn retain_thumbnails(&mut self, wanted: &HashSet<PathBuf>) {
        for block in &mut self.thumb_blocks {
            if block.as_ref().is_some_and(|path| !wanted.contains(path)) {
                *block = None;
            }
        }
        self.thumbs.retain(|path, _| wanted.contains(path));
    }

    /// The layer holding one picture, if it is resident.
    pub fn scenery(&self, of: &crate::art::Sight) -> Option<u32> {
        self.scenery_layers
            .iter()
            .position(|held| held.as_ref() == Some(of))
            .map(|layer| layer as u32)
    }

    /// Put a picture into a free layer, with all of its halvings.
    ///
    /// Nothing is evicted to make room: a layer is only free once the shell
    /// has said it no longer wants what is in it, and a picture arriving for a
    /// display that has since moved on must not take the layer out from under
    /// the picture somebody is looking at. Answers false when there is no room,
    /// which leaves that display's wallpaper as it was.
    pub fn put_scenery(&mut self, of: &crate::art::Sight, scenery: &crate::art::Scenery) -> bool {
        if self.scenery(of).is_some() {
            return false;
        }
        let Some(layer) = self.scenery_layers.iter().position(Option::is_none) else {
            tracing::debug!(?of, "no free layer for that picture; the wallpaper stays");
            return false;
        };
        for (level, pixels) in scenery.levels.iter().enumerate() {
            let level = level as u32;
            let (width, height) = crate::art::Scenery::size(level);
            if pixels.len() != (width * height * 4) as usize {
                tracing::warn!(?of, level, "that rung is not the size it should be");
                return false;
            }
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.scenery_texture,
                    mip_level: level,
                    origin: wgpu::Origin3d {
                        x: 0,
                        y: 0,
                        z: layer as u32,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        }
        self.scenery_layers[layer] = Some(of.clone());
        true
    }

    /// What the shader is told the wallpaper is: nought or one for the shell's
    /// own two materials, two for the user's own picture.
    ///
    /// Two only while there is really a picture to read. The setting can say
    /// Custom for a whole session in which no frame ever arrives — a file on a
    /// drive that is not plugged in, a format nothing on this machine decodes,
    /// or simply the first tenth of a second while it is being opened — and the
    /// honest thing to draw in the meantime is the shell's own wallpaper rather
    /// than an empty texture. Deciding it here means the shader never has to ask
    /// and neither does anything else.
    fn wallpaper_flag(&self) -> f32 {
        let flag = crate::theme::style_flag(crate::theme::Part::Wallpaper);
        if flag > 1.5 && self.paper.is_none() {
            return 0.0;
        }
        flag
    }

    /// The shape of the picture behind everything — its width over its height —
    /// which is what the crop to a display of another shape is worked out from.
    /// Nought where there is no picture, which is a shape the shader never asks
    /// about because it is only read on the branch [`Gpu::wallpaper_flag`]
    /// opens.
    fn paper_shape(&self) -> f32 {
        self.paper
            .as_ref()
            .map(|paper| paper.width as f32 / paper.height.max(1) as f32)
            .unwrap_or(0.0)
    }

    /// Put one frame of the user's own wallpaper on the GPU, with its chain of
    /// halvings, and hand back the buffer it came in.
    ///
    /// The buffer is returned rather than dropped because a film hands over one
    /// of these every frame: the decoder fills it again instead of allocating
    /// eight megabytes sixty times a second. See [`crate::paper::Paper::take`].
    ///
    /// A frame of a different size to the last one is a new texture and a new
    /// bind group — which is what a wallpaper being *changed* is, and what a
    /// film's first frame is. Every frame after that writes into the texture
    /// already there.
    pub fn put_paper(&mut self, frame: crate::paper::Frame) -> Option<Vec<u8>> {
        let (width, height) = (frame.width.max(1), frame.height.max(1));
        if frame.pixels.len() < (width * height * 4) as usize {
            tracing::warn!(
                width,
                height,
                "that wallpaper frame is not the size it says"
            );
            return Some(frame.pixels);
        }
        if !self
            .paper
            .as_ref()
            .is_some_and(|paper| paper.width == width && paper.height == height)
        {
            self.paper = Some(Paper::new(
                &self.device,
                &self.sample_layout,
                &self.sampler,
                width,
                height,
            ));
            self.scenery_bind_group = scenery_group(
                &self.device,
                &self.scenery_layout,
                &self.scenery_texture,
                &self.sampler,
                self.paper.as_ref().map(|paper| &paper.texture),
                &self.blank_paper,
            );
        }
        let paper = self.paper.as_ref()?;
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &paper.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &frame.pixels,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        // The halvings, off the rung above each time — the same chain the
        // backdrop's frost reads, drawn by the same pipeline.
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("custom wallpaper blur"),
            });
        for rung in 1..paper.rungs.len() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("custom wallpaper blur"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &paper.rungs[rung],
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
            pass.set_pipeline(&self.paper_downsample_pipeline);
            pass.set_bind_group(0, &paper.sources[rung - 1], &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit(Some(encoder.finish()));
        Some(frame.pixels)
    }

    /// Stop drawing the user's own wallpaper: the setting has been changed back
    /// to one of the shell's own materials, or the file turned out not to be one
    /// this shell can draw.
    ///
    /// The texture goes with it rather than being kept in case it is wanted
    /// again — it is a display's worth of pixels, and getting it back costs one
    /// upload of a file that is on the disk.
    pub fn drop_paper(&mut self) {
        if self.paper.take().is_none() {
            return;
        }
        self.scenery_bind_group = scenery_group(
            &self.device,
            &self.scenery_layout,
            &self.scenery_texture,
            &self.sampler,
            None,
            &self.blank_paper,
        );
    }

    /// Give up every layer whose picture no display is showing.
    ///
    /// The same policy as the thumbnails, and it has to be: what these hold is
    /// what is on screen, and a picture the cursor has left is one nothing will
    /// draw again until it is asked for. Getting it back costs a read of a file
    /// that is on the disk by then.
    pub fn retain_scenery(&mut self, wanted: &HashSet<crate::art::Sight>) {
        for layer in &mut self.scenery_layers {
            if layer.as_ref().is_some_and(|of| !wanted.contains(of)) {
                *layer = None;
            }
        }
    }

    /// The top-left cell of one block of a band: which band it starts at, how
    /// many cells a block of it is a side, and which block.
    ///
    /// An associated function taking the atlas width rather than a method, so
    /// that a block can be worked out while the texture behind it is being
    /// written to — and so the arithmetic that has to agree with [`Gpu::uv_for`]
    /// can be checked without a GPU.
    fn band_cell(cells_per_row: u32, band: u32, cells: u32, block: usize) -> (u32, u32) {
        let per_row = (cells_per_row / cells).max(1);
        let block = block as u32;
        ((block % per_row) * cells, band + (block / per_row) * cells)
    }

    /// Draw one frame.
    ///
    /// `backdrop` is the animated background pass: how blurred, and which
    /// region of the surface it fills. `None` leaves the surface transparent
    /// wherever the scene does not paint — the overlay drawn over a running
    /// application, or a surface whose backdrop lives on another surface.
    ///
    /// `hero` is what the wallpaper currently *is* on this display, and it is
    /// wanted whether or not this surface draws the backdrop pass: the panes
    /// of glass in the scene re-create the wallpaper to refract it, so a
    /// surface handed the wrong one shows a pane full of a picture that is not
    /// behind it.
    pub fn render(
        &mut self,
        target: &mut Target,
        quads: &[Quad],
        texts: &[Text],
        time: f32,
        backdrop: Option<Backdrop>,
        hero: Hero,
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
                hero: hero.packed(),
                style: [
                    self.wallpaper_flag(),
                    crate::theme::style_flag(crate::theme::Part::Icons),
                    self.paper_shape(),
                    0.0,
                ],
            }),
        );

        let instances: Vec<Instance> = quads
            .iter()
            .map(|q| Instance {
                rect: [q.x, q.y, q.w, q.h],
                uv: cropped(self.uv_for(q.slot), q.crop),
                color: q.color,
                shape: [q.radius, q.border, q.notch, q.frost],
                material: [q.thickness, q.behind, q.gloss, q.fade],
                corner: q.corner,
                face_curve: q.face_curve,
                drain: q.drain,
                cut: q.clip.map_or(UNCUT, |[x, y, w, h]| [x, y, x + w, y + h]),
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

        let mut areas: Vec<TextArea<'_>> = Vec::with_capacity(target.text_buffers.len());
        for ((_, buffer), text) in target.text_buffers.iter().zip(texts) {
            let bounds = text_bounds(text);
            // The halo first, so the letters land on top of their own shade.
            // Every copy is the same shaped buffer moved a little, which is
            // why a ring costs no shaping and cannot drift out of step with
            // the run it belongs to.
            for (dx, dy, shade) in halo_copies(text.size, text.halo) {
                areas.push(TextArea {
                    buffer,
                    left: text.x + dx,
                    top: text.y + dy,
                    scale: 1.0,
                    bounds,
                    default_color: TextColor::rgba(0, 0, 0, (shade * 255.0) as u8),
                    custom_glyphs: &[],
                });
            }
            areas.push(TextArea {
                buffer,
                left: text.x,
                top: text.y,
                scale: 1.0,
                bounds,
                default_color: TextColor::rgba(
                    (text.color[0] * 255.0) as u8,
                    (text.color[1] * 255.0) as u8,
                    (text.color[2] * 255.0) as u8,
                    (text.color[3] * 255.0) as u8,
                ),
                custom_glyphs: &[],
            });
        }

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
                pass.set_bind_group(3, &self.scenery_bind_group, &[]);
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
            pass.set_bind_group(3, &self.scenery_bind_group, &[]);
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
        let per_col = self.atlas_cells_per_col.max(1);
        let col = slot % per_row;
        let row = slot / per_row;
        let step_x = 1.0 / per_row as f32;
        let step_y = 1.0 / per_col as f32;

        if slot == SOLID_SLOT {
            // Sample the middle of the white cell so filtering never bleeds in
            // a neighbouring icon's edge.
            let (cx, cy) = (step_x * 0.5, step_y * 0.5);
            return [cx, cy, cx, cy];
        }

        // A picture covers only part of its block — a wide one leaves the
        // bottom of it empty, and one smaller than the block leaves the right
        // of it empty too — so what is sampled is the picture, not the block.
        // Taken from what was actually written, because how much of the block
        // a picture covers does not follow from its shape: see
        // [`Thumb::covers`].
        let inset_x = step_x * (1.5 / CELL as f32);
        let inset_y = step_y * (1.5 / CELL as f32);
        if let Some((picture, cells)) = self.picture_at(slot) {
            return block_uv(
                [col as f32 * step_x, row as f32 * step_y],
                [step_x, step_y],
                [inset_x, inset_y],
                picture.covers,
                cells,
            );
        }

        // Keep the sample footprint well inside the cell: bilinear filtering
        // reaches half a texel beyond the sampled area, so anything closer to
        // the border blends in the neighbouring cell. The glow sits next to
        // the opaque-white solid cell, where that bleed used to draw a bright
        // hairline along the edge of every (heavily magnified) glow quad.
        [
            col as f32 * step_x + inset_x,
            row as f32 * step_y + inset_y,
            (col + 1) as f32 * step_x - inset_x,
            (row + 1) as f32 * step_y - inset_y,
        ]
    }

    /// The part of a block `cells` cells a side a picture of `width` by
    /// `height` pixels covers, along each axis. See [`Thumb::covers`].
    fn block_coverage(width: u32, height: u32, cells: u32) -> [f32; 2] {
        let edge = (cells * CELL) as f32;
        [width as f32 / edge, height as f32 / edge]
    }

    /// The picture occupying `slot` and how big its block is, if the slot is
    /// in a band that holds pictures at all.
    ///
    /// Looked up by slot rather than kept beside it, because a [`Quad`] can
    /// only carry a slot number and the bands are small. Which band decides
    /// the block size, and getting that wrong samples the wrong rectangle of
    /// the atlas — a logo drawn with a thumbnail's block would come out as its
    /// top-left corner blown up over half the display.
    fn picture_at(&self, slot: u32) -> Option<(Thumb, u32)> {
        let row = slot / self.atlas_cells_per_row.max(1);
        if row >= self.logo_band {
            return self
                .logos
                .values()
                .copied()
                .find(|logo| logo.slot == slot)
                .map(|logo| (logo, LOGO_CELLS));
        }
        if row < self.thumb_band {
            return None;
        }
        self.thumbs
            .values()
            .copied()
            .find(|thumb| thumb.slot == slot)
            .map(|thumb| (thumb, THUMB_CELLS))
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
                hero: [0.0; 4],
                style: [0.0; 4],
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

/// The rectangle a run is allowed to put pixels in: its own layout box, cut
/// down by whatever is standing over it.
///
/// Glyphon clips glyph by glyph against this and trims the partly covered ones
/// against their own texture, so a label disappearing under a panel is cut at
/// the panel's edge rather than at the last whole letter before it.
fn text_bounds(text: &Text) -> TextBounds {
    // The run's own box, opened out by however far its ring reaches. The ring
    // is drawn by shifting the letters, so a box cut to where the letters
    // *are* would shave the left and top of it off — and a ring missing two
    // sides is a drop shadow nobody asked for.
    let room = if text.halo > 0.0 {
        (text.size * HALO_RING).ceil()
    } else {
        0.0
    };
    // Tall enough for every line the run is allowed, or a cut run would have
    // its second line sliced off by the box drawn for a one-line label.
    let depth = (text.size * 2.0).max(text.size * 1.25 * text.lines.max(1) as f32);
    let mut bounds = TextBounds {
        left: (text.x - room).floor() as i32,
        top: (text.y - room).floor() as i32,
        right: (text.x + text.max_width + room).ceil() as i32,
        bottom: (text.y + depth + room).ceil() as i32,
    };
    if let Some([x, y, w, h]) = text.clip {
        bounds.left = bounds.left.max(x.floor() as i32);
        bounds.top = bounds.top.max(y.floor() as i32);
        bounds.right = bounds.right.min((x + w).ceil() as i32);
        bounds.bottom = bounds.bottom.min((y + h).ceil() as i32);
    }
    bounds
}

/// Lay `content` into `buffer` in this run's face, at its size, in its box.
///
/// `cap` is how many lines the layout may take before it puts an ellipsis on
/// the last of them: the run's own allowance, or one where the breaks have
/// already been written into the content and each line is a line of its own —
/// see [`with_breaks_written_in`].
///
/// One line, and an ellipsis where the rest of it would have been.
///
/// Every run the shell draws is a label — a clock, a title, the name of
/// something — and each is drawn against a box one line tall, which the bounds
/// around it clip to. A run left to wrap therefore does not get a second line:
/// it gets the *top half* of one, sliced through the letters, under a first
/// line that gave no sign it was going to overflow. Window titles are exactly
/// the text this happens to. They routinely carry a document, a page or a whole
/// working directory, and none of them stop at the edge of the sidebar.
///
/// Shaping is what knows how wide the run really is, so the ellipsis is put in
/// here rather than guessed at from a character count by the caller: the label
/// is cut where the box ends and nowhere earlier.
fn lay_out(
    font_system: &mut FontSystem,
    buffer: &mut TextBuffer,
    text: &Text,
    content: &str,
    cap: u8,
) {
    buffer.set_ellipsize(glyphon::cosmic_text::Ellipsize::End(
        glyphon::cosmic_text::EllipsizeHeightLimit::Lines(cap.max(1) as usize),
    ));
    let attrs = Attrs::new()
        .family(Family::Name(UI_FONT))
        .weight(if text.bold {
            Weight::BOLD
        } else {
            Weight::NORMAL
        });
    buffer.set_text(content, &attrs, Shaping::Advanced, None);
    let align = match text.align {
        TextAlign::Left => None,
        TextAlign::Center => Some(glyphon::cosmic_text::Align::Center),
        TextAlign::Right => Some(glyphon::cosmic_text::Align::Right),
    };
    for line in buffer.lines.iter_mut() {
        line.set_align(align);
    }
    buffer.shape_until_scroll(font_system, false);
}

/// `content` with the breaks a laid-out `buffer` chose written into it, or
/// `None` where it is already laid out the way it should be drawn.
///
/// `None` for all but a handful of runs in a session: only one whose lines the
/// layout began with a blank needs this, and only a run that wrapped at all can
/// be one. It is also `None` for anything this cannot be sure of — a line the
/// layout drew no glyphs on, or lines that do not run through the content from
/// its start in order, which is what a run of mixed direction looks like from
/// here. Leaving those exactly as they were is the whole point: this is a
/// blemish being taken off a layout, not a second layout.
fn with_breaks_written_in(buffer: &TextBuffer, content: &str) -> Option<String> {
    let mut starts = Vec::new();
    for run in buffer.layout_runs() {
        // The logical start, not the leftmost: the glyphs of a line are in the
        // order they are drawn in.
        starts.push(run.glyphs.iter().map(|glyph| glyph.start).min()?);
    }
    if starts.first() != Some(&0) || starts.windows(2).any(|pair| pair[0] >= pair[1]) {
        return None;
    }
    let begins_blank = |at: &usize| {
        content[*at..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
    };
    if !starts.iter().skip(1).any(begins_blank) {
        return None;
    }

    let mut broken = String::with_capacity(content.len());
    for (index, start) in starts.iter().enumerate() {
        let end = starts.get(index + 1).copied().unwrap_or(content.len());
        // The blank a line was broken at belongs to neither side of the break,
        // and a blank at the end of a line was never drawn in the first place.
        // The first line keeps whatever it was given to begin with: leading
        // space there is somebody's own.
        let line = match index {
            0 => content[*start..end].trim_end(),
            _ => content[*start..end].trim(),
        };
        if index > 0 {
            broken.push('\n');
        }
        broken.push_str(line);
    }
    Some(broken)
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

        let lines = text.lines.max(1) as f32;
        let mut buffer = TextBuffer::new(font_system, Metrics::new(text.size, text.size * 1.25));
        buffer.set_size(
            Some(text.max_width),
            Some((text.size * 2.0).max(text.size * 1.25 * lines)),
        );
        lay_out(font_system, &mut buffer, text, &text.content, text.lines);
        // A wrapped line never begins with the space it was wrapped at.
        //
        // The layout puts the ellipsis on the last line the run is allowed, and
        // it builds that line from the break *including* the blank the break
        // fell on — so the run's last line starts a space in from every line
        // above it, and a paragraph reads as though somebody had indented its
        // final line by hand. It is the announcement panel this shows up on
        // most, whose rows are three lines of somebody else's sentence.
        //
        // The cure is to hand the layout the breaks it chose rather than argue
        // with how it draws them: the run is laid out once to find out where
        // the lines fall, and if any of them begins with a blank it is laid out
        // again with those breaks written in and the blanks taken off the ends
        // of the lines they belong to. Every line is then a line of its own —
        // hence a run allowed exactly one apiece, which is also what keeps the
        // last one ellipsized.
        if let Some(broken) = with_breaks_written_in(&buffer, &text.content) {
            lay_out(font_system, &mut buffer, text, &broken, 1);
        }

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

/// Geometry of the atlas used while only the handed-off wallpaper is visible.
///
/// No runtime band is useful before the complete catalogue lands: the startup
/// gate prevents anything from writing to one, and the background pass uses no
/// atlas sample at all. The two procedural cells remain because the renderer's
/// quad pipeline always has a valid binding and because they are the exact
/// first two cells the settled atlas will carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WallpaperAtlasGeometry {
    cells_per_row: u32,
    cells_per_col: u32,
    late_first: u32,
    late_cells: usize,
    thumb_band: u32,
    thumb_blocks: usize,
    logo_band: u32,
    logo_blocks: usize,
}

const fn wallpaper_atlas_geometry() -> WallpaperAtlasGeometry {
    WallpaperAtlasGeometry {
        cells_per_row: RESERVED_SLOTS,
        cells_per_col: 1,
        late_first: RESERVED_SLOTS,
        late_cells: 0,
        thumb_band: 1,
        thumb_blocks: 0,
        logo_band: 1,
        logo_blocks: 0,
    }
}

/// Build only the valid texture binding needed for the wallpaper's first
/// frames. [`build_atlas`] remains the sole builder of the settled atlas, so
/// catalogue slot ordering and every runtime band retain their old layout.
fn build_wallpaper_atlas(device: &wgpu::Device, queue: &wgpu::Queue) -> Atlas {
    let geometry = wallpaper_atlas_geometry();
    let width = geometry.cells_per_row * CELL;
    let height = geometry.cells_per_col * CELL;
    let pixels = procedural_atlas_pixels(geometry.cells_per_row, geometry.cells_per_col);
    let texture = upload_atlas(device, queue, width, height, &pixels);
    tracing::debug!(width, height, "built wallpaper atlas");

    Atlas {
        texture,
        slots: HashMap::new(),
        cells_per_row: geometry.cells_per_row,
        cells_per_col: geometry.cells_per_col,
        late_first: geometry.late_first,
        late_cells: geometry.late_cells,
        thumb_band: geometry.thumb_band,
        thumb_blocks: geometry.thumb_blocks,
        logo_band: geometry.logo_band,
        logo_blocks: geometry.logo_blocks,
    }
}

/// Paint the two cells common to both atlas shapes and leave every other byte
/// clear. Extracted from the settled builder without changing its arithmetic,
/// so replacing the texture cannot also change the solid or glow sprites.
fn procedural_atlas_pixels(cells_per_row: u32, cells_per_col: u32) -> Vec<u8> {
    assert!(cells_per_row > 0 && cells_per_row * cells_per_col >= RESERVED_SLOTS);
    let width = cells_per_row * CELL;
    let height = cells_per_col * CELL;
    let mut pixels = vec![0u8; (width * height * 4) as usize];

    // Slot 0: opaque white.
    for y in 0..CELL {
        for x in 0..CELL {
            let offset = ((y * width + x) * 4) as usize;
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
            let offset = (((glow_y + y) * width + glow_x + x) * 4) as usize;
            pixels[offset..offset + 4].copy_from_slice(&[255, 255, 255, alpha]);
        }
    }

    pixels
}

fn upload_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("icon atlas"),
        size: wgpu::Extent3d {
            width,
            height,
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
        pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(width * 4),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    texture
}

/// Pack every icon into one texture, sized to fit.
///
/// Slot 0 is filled with opaque white so the same pipeline can draw untextured
/// rectangles by tinting it, and slot 1 holds the procedural selection glow.
fn build_atlas(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    icons: Vec<(String, Icon)>,
) -> anyhow::Result<Atlas> {
    // The late band is counted in here and left empty, so that the rows are
    // sized for it once rather than the atlas being rebuilt the first time a
    // program sends an icon nobody had heard of.
    let needed = icons.len() as u32 + RESERVED_SLOTS + LATE_CELLS;
    let mut cells_per_row = (needed as f64).sqrt().ceil() as u32;
    cells_per_row = cells_per_row.max(2);
    // Even, so the thumbnail band divides into whole blocks.
    cells_per_row += cells_per_row % THUMB_CELLS;

    let max_dim = device.limits().max_texture_dimension_2d;
    let max_cells_per_row = (max_dim / CELL).max(1);
    if cells_per_row > max_cells_per_row {
        tracing::warn!(
            icons = icons.len(),
            "too many icons for one atlas; some will be drawn without one"
        );
        cells_per_row = max_cells_per_row;
    }

    // The icons fill whole rows from the top; the thumbnails get a band of
    // their own under them, and the logos a band under that. Bands rather than
    // the leftovers of the icon rows, because both are blocks of cells and
    // have to be aligned to one — and two bands rather than one because their
    // blocks are different sizes and a block of either has to start on a
    // multiple of its own edge.
    let icon_rows = needed.div_ceil(cells_per_row);
    let blocks_per_row = (cells_per_row / THUMB_CELLS).max(1);
    let mut band_rows = THUMB_BLOCKS.div_ceil(blocks_per_row) * THUMB_CELLS;
    let max_rows = (max_dim / CELL).max(1);
    if icon_rows + band_rows > max_rows {
        band_rows = max_rows.saturating_sub(icon_rows) / THUMB_CELLS * THUMB_CELLS;
        tracing::warn!(band_rows, "the atlas has little room left for thumbnails");
    }
    let logos_per_row = (cells_per_row / LOGO_CELLS).max(1);
    let mut logo_rows = LOGO_BLOCKS.div_ceil(logos_per_row) * LOGO_CELLS;
    let taken_rows = icon_rows + band_rows;
    if taken_rows + logo_rows > max_rows {
        logo_rows = max_rows.saturating_sub(taken_rows) / LOGO_CELLS * LOGO_CELLS;
        // Not a failure. A session whose atlas has no room left for these
        // draws a launching game under its own name instead of under its
        // artwork, which is what the splash falls back to for every game Valve
        // has no logo for anyway.
        tracing::warn!(logo_rows, "the atlas has little room left for game logos");
    }
    let logo_band = taken_rows;
    let logos = (logos_per_row * (logo_rows / LOGO_CELLS)).min(LOGO_BLOCKS);
    let cells_per_col = taken_rows + logo_rows;

    let width = cells_per_row * CELL;
    let height = cells_per_col * CELL;
    let mut pixels = procedural_atlas_pixels(cells_per_row, cells_per_col);

    let capacity = cells_per_row * icon_rows;
    let mut slots = HashMap::new();
    let mut taken = RESERVED_SLOTS;

    for (index, (name, icon)) in icons.into_iter().enumerate() {
        let slot = index as u32 + RESERVED_SLOTS;
        if slot >= capacity {
            break;
        }
        taken = slot + 1;
        let cell_x = (slot % cells_per_row) * CELL;
        let cell_y = (slot / cells_per_row) * CELL;

        // Icons are decoded at CELL already, but guard against a short buffer.
        let size = icon.size.min(CELL);
        for y in 0..size {
            let src = (y * icon.size * 4) as usize;
            let dst = (((cell_y + y) * width + cell_x) * 4) as usize;
            let len = (size * 4) as usize;
            if src + len <= icon.rgba.len() && dst + len <= pixels.len() {
                pixels[dst..dst + len].copy_from_slice(&icon.rgba[src..src + len]);
            }
        }
        slots.insert(name, slot);
    }

    let texture = upload_atlas(device, queue, width, height, &pixels);

    let blocks = (blocks_per_row * (band_rows / THUMB_CELLS)).min(THUMB_BLOCKS);
    tracing::debug!(
        width,
        height,
        cells_per_row,
        icons = slots.len(),
        thumbnails = blocks,
        logos,
        "built icon atlas"
    );
    // Whatever is left of the icon rows after the startup icons, capped at the
    // band that was asked for. It is the leftovers rather than a fixed range
    // because a machine with more applications than the atlas can hold has
    // already lost cells to the `break` above, and a band pointing past the
    // last row would be cells that are not there.
    let late_first = taken;
    let late_cells = capacity.saturating_sub(late_first).min(LATE_CELLS);
    if late_cells < LATE_CELLS {
        tracing::warn!(
            late_cells,
            "little room left for icons that arrive with an announcement"
        );
    }

    Ok(Atlas {
        texture,
        slots,
        cells_per_row,
        cells_per_col,
        late_first,
        late_cells: late_cells as usize,
        thumb_band: icon_rows,
        thumb_blocks: blocks as usize,
        logo_band,
        logo_blocks: logos as usize,
    })
}

/// The atlas as it comes out of [`build_atlas`].
struct Atlas {
    texture: wgpu::Texture,
    slots: HashMap<String, u32>,
    cells_per_row: u32,
    cells_per_col: u32,
    /// First cell of the band held for icons that arrive with an announcement,
    /// and how many of them there are — see [`LATE_CELLS`].
    late_first: u32,
    late_cells: usize,
    /// First cell row of the thumbnail band.
    thumb_band: u32,
    /// How many thumbnails fit in it.
    thumb_blocks: usize,
    /// And the same for the band of game logos under it.
    logo_band: u32,
    logo_blocks: usize,
}

/// The texture rectangle of a picture inside its block.
///
/// `origin` and `step` are the block's top-left corner and one cell, both in
/// texture coordinates; `inset` keeps the sample footprint off the border so
/// bilinear filtering cannot reach into the next cell; `cells` is how many
/// cells a block of this band is a side.
///
/// Split out of [`Gpu::uv_for`] so the arithmetic can be checked without a
/// GPU. It is worth checking on its own: getting it wrong draws the picture
/// into a corner of its card and leaves the rest empty, which no other test the
/// shell has can see, and which looks enough like a deliberate mount that it
/// went unnoticed.
fn block_uv(
    origin: [f32; 2],
    step: [f32; 2],
    inset: [f32; 2],
    covers: [f32; 2],
    cells: u32,
) -> [f32; 4] {
    let width = step[0] * cells as f32 * covers[0];
    let height = step[1] * cells as f32 * covers[1];
    [
        origin[0] + inset[0],
        origin[1] + inset[1],
        origin[0] + width - inset[0],
        origin[1] + height - inset[1],
    ]
}

/// A picture resident in one of the atlas's blocked bands: a thumbnail of one
/// of the user's own files, a Steam cover, or a game's logo.
#[derive(Debug, Clone, Copy)]
pub struct Thumb {
    /// The cell its block starts at, which is what a [`Quad`] carries.
    pub slot: u32,
    /// Width over height of the picture itself, so the row can draw a card of
    /// the same shape rather than squashing a photograph into a square.
    pub aspect: f32,
    /// How much of the block the picture actually covers, along each axis, as
    /// a fraction of the block's edge.
    ///
    /// Kept rather than worked back out of [`Self::aspect`], which is what this
    /// used to do and which was wrong for every thumbnail that is not 256 on
    /// its long side. The freedesktop `large` directory is a ceiling, not a
    /// size: a picture smaller than the ceiling is stored at its own size, and
    /// so is one another desktop wrote there — the cache on the machine this
    /// was found on is largely 160 across, from a file manager that thumbnails
    /// to 160 and files it under `large` like everything else. Those went into
    /// a 256-wide block at 160 wide and were then sampled as if they filled it,
    /// so five eighths of the picture was drawn into the top-left corner of the
    /// card and the rest of the card was the empty part of the block.
    pub covers: [f32; 2],
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The transition atlas is a valid binding and nothing more: exactly the
    /// two procedural cells in one row, with no storage reserved for pictures
    /// the startup gate cannot yet admit.
    #[test]
    fn the_wallpaper_atlas_has_only_its_two_procedural_cells() {
        let geometry = wallpaper_atlas_geometry();
        assert_eq!(geometry.cells_per_row, RESERVED_SLOTS);
        assert_eq!(geometry.cells_per_col, 1);
        assert_eq!(geometry.late_cells, 0);
        assert_eq!(geometry.thumb_blocks, 0);
        assert_eq!(geometry.logo_blocks, 0);

        let bytes = procedural_atlas_pixels(geometry.cells_per_row, geometry.cells_per_col);
        assert_eq!(bytes.len(), (RESERVED_SLOTS * CELL * CELL * 4) as usize);
        let width = geometry.cells_per_row * CELL;
        let pixel = |x: u32, y: u32| {
            let at = ((y * width + x) * 4) as usize;
            &bytes[at..at + 4]
        };
        for y in 0..CELL {
            for x in 0..CELL {
                assert_eq!(pixel(x, y), [255, 255, 255, 255]);
            }
        }
        assert!(pixel(CELL + CELL / 2, CELL / 2)[3] > 0);
    }

    /// What is sampled has to be what was written, whatever size the picture
    /// turned out to be.
    ///
    /// A thumbnail is written into the top-left of a fixed 256-pixel block at
    /// its own size, so how much of the block it covers is a fact about the
    /// file and not about its shape. Deriving it from the aspect instead — the
    /// bug this pins — happens to be right for the pictures that do fill the
    /// block on their long side and wrong for every other one, which is why a
    /// column of thumbnails came out with some of its pictures correct and the
    /// rest tucked into the corner of their cards.
    #[test]
    fn a_thumbnail_is_sampled_where_its_pixels_are() {
        let edge = (THUMB_CELLS * CELL) as f32;
        // One cell of a notional 32-cell-square atlas, and a block at its
        // origin, so the numbers below are the fractions themselves.
        let step = [1.0 / 32.0, 1.0 / 32.0];
        let block = [step[0] * THUMB_CELLS as f32, step[1] * THUMB_CELLS as f32];
        let sampled = |w: u32, h: u32| {
            let uv = block_uv(
                [0.0, 0.0],
                step,
                [0.0, 0.0],
                Gpu::block_coverage(w, h, THUMB_CELLS),
                THUMB_CELLS,
            );
            [uv[2] / block[0], uv[3] / block[1]]
        };

        // A picture that fills its block on the long side: the case the old
        // arithmetic got right, and it still is.
        let full = sampled(edge as u32, (edge / 16.0 * 9.0) as u32);
        assert!((full[0] - 1.0).abs() < 1e-5, "{full:?}");
        assert!((full[1] - 9.0 / 16.0).abs() < 1e-3, "{full:?}");

        // And one that does not, which is most of what a real cache holds —
        // 160 across is what the machine this was found on had. Both axes have
        // to shrink; the shape is the same 16:9 as above, so an aspect is no
        // help in telling the two apart.
        let small = sampled(160, 90);
        assert!((small[0] - 160.0 / edge).abs() < 1e-5, "{small:?}");
        assert!((small[1] - 90.0 / edge).abs() < 1e-5, "{small:?}");
        assert!(
            small[0] < full[0] && small[1] < full[1],
            "a smaller picture must sample a smaller part of the block"
        );

        // A tall picture is the same rule the other way up.
        let tall = sampled(91, 160);
        assert!((tall[0] - 91.0 / edge).abs() < 1e-5, "{tall:?}");
        assert!((tall[1] - 160.0 / edge).abs() < 1e-5, "{tall:?}");
    }

    /// The atlas now has two sizes of block in it, and which one a slot
    /// belongs to is the whole of what the sampling depends on.
    ///
    /// Reading a logo with a thumbnail's block would take the top 256 pixels
    /// of a 640-pixel wordmark and stretch them across a third of the display;
    /// reading a thumbnail with a logo's would draw a photograph into the
    /// corner of its card. Neither is a crash and neither is visible in any
    /// number the layout can be asked for, so it is pinned here.
    #[test]
    fn a_block_is_sampled_at_the_size_of_the_band_it_is_in() {
        let step = [1.0 / 32.0, 1.0 / 32.0];
        // A logo at Valve's ceiling fills its block exactly, so it is sampled
        // to the edge of it.
        let covers = Gpu::block_coverage(crate::art::LOGO_SIZE, 360, LOGO_CELLS);
        assert!((covers[0] - 1.0).abs() < 1e-6, "{covers:?}");
        let uv = |cells| block_uv([0.0, 0.0], step, [0.0, 0.0], covers, cells)[2];
        assert!(
            (uv(LOGO_CELLS) - step[0] * LOGO_CELLS as f32).abs() < 1e-6,
            "a logo must be sampled to the edge of its own block"
        );
        // The very same coverage read as a thumbnail's block is two fifths of
        // the texture, which is what the block size travelling with the
        // picture is there to prevent.
        assert!(uv(LOGO_CELLS) > uv(THUMB_CELLS));
    }

    /// Blocks of a band tile it left to right and then downwards, from the row
    /// the band starts at — and never overlap, whatever size they are.
    #[test]
    fn the_blocks_of_a_band_are_laid_out_inside_it() {
        let cells_per_row = 16;
        let band = 7;
        for cells in [THUMB_CELLS, LOGO_CELLS] {
            let per_row = cells_per_row / cells;
            let mut seen: Vec<(u32, u32)> = Vec::new();
            for block in 0..(per_row as usize * 3) {
                let (col, row) = Gpu::band_cell(cells_per_row, band, cells, block);
                assert!(row >= band, "a block above its own band");
                assert!(col + cells <= cells_per_row, "a block off the right edge");
                assert_eq!(col % cells, 0, "a block not aligned to its own edge");
                assert_eq!((row - band) % cells, 0);
                assert!(
                    !seen.iter().any(|&(c, r)| c == col && r == row),
                    "two blocks in one place"
                );
                seen.push((col, row));
            }
        }
    }

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

    /// The shell's own two faces and nothing else, so what a run measures is
    /// the same on any machine — the system's fonts differ from one to the
    /// next, and a test that shaped with them would be measuring the machine.
    fn shell_fonts() -> FontSystem {
        shell_faces()
    }

    /// Every character cut from the shell's own face — the corner's clock and an
    /// index's headings — ships as a measurement of its own shape, inside its
    /// own cell, with room round it for the shadow.
    ///
    /// The letters are held to the same three things a built-in glyph is — see
    /// `icons::tests::a_glyph_can_ship_as_the_shape_of_itself` — with one
    /// difference, which is the ink share. A glyph has to be a mark on a space
    /// and covers at least a twentieth of its cell; a colon is two dots in an em
    /// and covers a fiftieth, and that is right. What matters here is that it is
    /// *there*, that it is inside its cell, and that the field is a distance.
    ///
    /// The margin is what says the two sets need two boxes. `Q` is the letter
    /// that found it: its tail hangs a tenth of an em below the baseline, and in
    /// the box the clock's digits are centred in it ended in a straight cut down
    /// the bottom of its cell.
    #[test]
    fn every_letter_cut_from_the_face_is_a_shape_in_its_cell() {
        let fields = letter_fields();
        let expected: Vec<&str> = LETTER_SET
            .iter()
            .chain(crate::icons::INDEX_LETTERS.iter())
            .map(|(_, name)| *name)
            .chain(std::iter::once(crate::icons::INDEX_MARK))
            .filter(|name| !name.is_empty())
            .collect();
        let names: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names, expected,
            "the clock's own characters, the index's headings and its mark, in order"
        );

        let size = CELL as usize;
        for (name, icon) in &fields {
            assert_eq!(icon.size, CELL);
            assert_eq!(icon.rgba.len(), size * size * 4);
            let at = |x: usize, y: usize| {
                let stored = f32::from(icon.rgba[(y * size + x) * 4 + 3]) / 255.0;
                (stored - 0.5) * 2.0 * crate::icons::SDF_RANGE * size as f32
            };

            // Signed: some of the cell is letter and some of it is air. A
            // character that came out entirely one way is a mask that landed
            // outside its cell or a face that drew nothing.
            let inside = (0..size * size)
                .filter(|i| at(i % size, i / size) < 0.0)
                .count();
            let share = inside as f32 / (size * size) as f32;
            assert!(
                (0.005..0.40).contains(&share),
                "{name} is {share:.3} letter, which is not a letter on a space"
            );

            // The margin the shadow is drawn in, which for a letter is what
            // keeps one from ending in a straight cut where its cell does. The
            // tightest of them is the slash, which is nearly a cap tall and
            // dips below the baseline.
            let edge = size / 20;
            for i in 0..size {
                for (x, y) in [
                    (i, edge),
                    (i, size - 1 - edge),
                    (edge, i),
                    (size - 1 - edge, i),
                ] {
                    assert!(
                        at(x.min(size - 1), y.min(size - 1)) > 0.0,
                        "{name} reaches its own edge at {x},{y}"
                    );
                }
            }

            // And it is a distance: one pixel of travel can only ever be one
            // pixel of distance.
            for y in 1..size - 1 {
                for x in 1..size - 1 {
                    let step = (at(x, y) - at(x + 1, y))
                        .abs()
                        .max((at(x, y) - at(x, y + 1)).abs());
                    assert!(step <= 1.35, "{name} steps {step} at {x},{y}");
                }
            }
        }
    }

    /// A run of the corner's letters laid out by adding up their advances is
    /// exactly as wide as the same letters shaped.
    ///
    /// Which is why the corner may lay itself out at all: the alternative is
    /// shaping the time on the thread that has a frame due, and the reason it is
    /// allowed is that no pair of characters in this set kerns. If a future face
    /// changed that, the clock would drift from the mark beside it and from the
    /// edge it is aligned against — so it is checked here rather than assumed.
    #[test]
    fn the_corners_run_is_as_wide_as_the_same_letters_shaped() {
        let advances = letter_advances();
        assert_eq!(
            advances.len(),
            LETTER_SET.len(),
            "every character of the clock has an advance",
        );

        let mut font_system = shell_faces();
        let size = 100.0;
        // The clock, and the charge that stands on the same line in the same
        // material — the per cent sign is in this set for that and nothing
        // else, so the pairs it makes are checked here with the rest.
        for content in [
            "8/19 10:02",
            "12/31 23:59",
            "1/1 0:00",
            "9/9 9:09",
            "100%",
            "96%",
            "7%",
            "0%",
        ] {
            let summed: f32 = content.chars().map(|c| advances[&c] * size).sum::<f32>();
            let mut buffer = TextBuffer::new(&mut font_system, Metrics::new(size, size * 1.25));
            buffer.set_size(None, None);
            let attrs = Attrs::new()
                .family(Family::Name(UI_FONT))
                .weight(Weight::NORMAL);
            buffer.set_text(content, &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(&mut font_system, false);
            let shaped: f32 = buffer
                .layout_runs()
                .map(|run| run.line_w)
                .fold(0.0, f32::max);
            assert!(
                (summed - shaped).abs() < 0.05,
                "{content:?} adds up to {summed} and shapes to {shaped}",
            );
        }
    }

    /// The corner puts its letters on the baseline the text pipeline would have
    /// put them on.
    ///
    /// The clock is drawn as quads now, so nothing forces the two to agree — and
    /// they have to, or the corner moves the day this is edited and every run
    /// drawn beside it in the ordinary way sits on a different line.
    #[test]
    fn the_clock_sits_on_the_line_the_text_pipeline_would_have_put_it_on() {
        let mut font_system = shell_faces();
        for size in [18.0f32, 24.0, 60.0] {
            let mut buffer = TextBuffer::new(&mut font_system, Metrics::new(size, size * 1.25));
            buffer.set_size(None, None);
            let attrs = Attrs::new()
                .family(Family::Name(UI_FONT))
                .weight(Weight::NORMAL);
            buffer.set_text("8/19 10:02", &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(&mut font_system, false);
            let baseline = buffer.layout_runs().next().expect("one line").line_y / size;
            assert!(
                (baseline - crate::ui::CORNER_CLOCK_BASELINE).abs() < 1e-3,
                "at {size} the pipeline's baseline is {baseline}, the corner's is {}",
                crate::ui::CORNER_CLOCK_BASELINE,
            );
        }
    }

    fn label(content: &str, max_width: f32) -> Text {
        Text {
            content: content.to_string(),
            x: 0.0,
            y: 0.0,
            size: 18.0,
            color: [1.0; 4],
            bold: false,
            max_width,
            align: TextAlign::Left,
            clip: None,
            halo: 0.0,
            lines: 1,
        }
    }

    /// A halo is a ring and not a drop shadow: it goes the whole way round the
    /// letters, evenly, and no copy of the run stands where the run itself
    /// does.
    #[test]
    fn a_halo_rings_the_letters_rather_than_falling_to_one_side() {
        let size = 20.0;
        let copies = halo_copies(size, 1.0);

        let reach = size * HALO_RING;
        for (dx, dy, shade) in &copies {
            let out = (dx * dx + dy * dy).sqrt();
            assert!(out > 0.0, "a copy on top of the run shades nothing");
            assert!(out <= reach + 1e-4, "{out} is further out than {reach}");
            assert!(
                *shade > 0.0 && *shade < 1.0,
                "shade, not a blackout: {shade}"
            );
        }

        // Evenly round, which is what "ring" means. A shadow that leaned would
        // show up here as a centre of gravity away from the letters.
        let (sum_x, sum_y) = copies
            .iter()
            .fold((0.0, 0.0), |(x, y), (dx, dy, _)| (x + dx, y + dy));
        assert!(sum_x.abs() < 1e-3 && sum_y.abs() < 1e-3, "{sum_x}, {sum_y}");

        // And close enough together, at every radius and every size the shell
        // draws at, to still be a shadow. This is the one that guards the
        // reach: widen a ring without letting its count follow and the copies
        // pull apart into a row of little words.
        for size in [14.0, 20.0, 23.0, 48.0] {
            let copies = halo_copies(size, 1.0);
            let mut around: std::collections::BTreeMap<i64, Vec<(f32, f32)>> = Default::default();
            for (dx, dy, _) in &copies {
                let radius = (dx * dx + dy * dy).sqrt();
                // Bucketed coarsely: a copy's distance is rebuilt from a sine
                // and a cosine, so one ring's copies do not all come back at
                // bit-identical radii. The true radius is kept and the bucket
                // is only how they are grouped.
                around
                    .entry((radius * 10.0).round() as i64)
                    .or_default()
                    .push((radius, dy.atan2(*dx)));
            }
            assert_eq!(around.len(), HALO_RINGS.len(), "one radius per ring");
            for (_, mut ring) in around {
                let radius = ring.iter().map(|(r, _)| *r).fold(0.0f32, f32::max);
                ring.sort_by(|a, b| a.1.total_cmp(&b.1));
                let angles: Vec<f32> = ring.iter().map(|(_, angle)| *angle).collect();
                let widest = angles
                    .windows(2)
                    .map(|pair| pair[1] - pair[0])
                    // Round the circle, from the last back to the first.
                    .chain(std::iter::once(
                        std::f32::consts::TAU - (angles[angles.len() - 1] - angles[0]),
                    ))
                    .fold(0.0f32, f32::max);
                assert!(
                    widest * radius <= HALO_SPACING + 1e-3,
                    "at size {size} the ring at {radius} leaves a gap of {}",
                    widest * radius
                );
            }
        }
    }

    /// A halo is asked for at a strength and drawn at it. Nothing else scales
    /// it — in particular not the run's own colour, which is what a caller uses
    /// to say a line is *quieter*, never that it should be harder to read.
    #[test]
    fn a_halo_is_drawn_at_the_strength_it_was_asked_for() {
        assert!(
            halo_copies(20.0, 0.0).is_empty(),
            "a run that asked for none"
        );
        assert!(halo_copies(20.0, -1.0).is_empty());

        let full = halo_copies(20.0, 1.0);
        let half = halo_copies(20.0, 0.5);
        assert_eq!(full.len(), half.len(), "it thins rather than shrinking");
        for (whole, part) in full.iter().zip(&half) {
            assert!((part.2 - whole.2 * 0.5).abs() < 1e-6);
            assert_eq!((whole.0, whole.1), (part.0, part.1));
        }

        // Asked for more than the rings can carry, every copy stops at solid
        // rather than running past it.
        for (_, _, shade) in halo_copies(20.0, 40.0) {
            assert_eq!(shade, 1.0);
        }
    }

    /// The box a haloed run is cut to has to make room for the ring — and stop
    /// making it where something is standing in front of the run.
    #[test]
    fn a_halo_gets_its_room_from_the_box_but_never_from_the_clip() {
        let plain = label("Download finished", 300.0);
        let ringed = Text {
            halo: 1.0,
            lines: 1,
            ..plain.clone()
        };
        let bare = text_bounds(&plain);
        let round = text_bounds(&ringed);
        assert!(
            round.left < bare.left && round.top < bare.top,
            "room above and to the left"
        );
        assert!(round.right > bare.right && round.bottom > bare.bottom);

        // A panel in front of the run cuts the ring exactly where it cuts the
        // letters. A halo that leaked out from under something standing over
        // it would be the outline of a word that is not there.
        let covered = Text {
            clip: Some([40.0, 4.0, 100.0, 20.0]),
            ..ringed
        };
        let cut = text_bounds(&covered);
        assert_eq!((cut.left, cut.top, cut.right, cut.bottom), (40, 4, 140, 24));
    }

    /// A run wider than its box is cut where the box ends, on the one line the
    /// bounds around it can show. What this replaces is the second line those
    /// bounds used to slice through the middle of — half the height of a word,
    /// under a first line that gave no sign there was more of it.
    #[test]
    fn a_label_too_long_for_its_box_is_ellipsized_onto_one_line() {
        let mut font_system = shell_fonts();
        let title = "(7) This $400 Handheld is replacing my Steamdeck OLED — Mozilla Firefox";
        let mut pool = Vec::new();
        shape_texts(&mut font_system, &mut pool, &[label(title, 200.0)]);

        let (_, buffer) = &pool[0];
        let runs: Vec<_> = buffer.layout_runs().collect();
        assert_eq!(runs.len(), 1, "a label is one line, never two");
        assert!(
            runs[0].line_w <= 200.0,
            "{} is wider than the box it was given",
            runs[0].line_w
        );
        // The ellipsis is shaped from a string of its own, so what says the
        // title was cut is how far into it the last glyph of the title reaches.
        let reached = runs[0].glyphs.iter().map(|glyph| glyph.end).max();
        assert!(
            reached.is_some_and(|reached| reached < title.len()),
            "the whole title was drawn after all"
        );
    }

    /// No line of a wrapped run begins with the space it was wrapped at.
    ///
    /// The layout builds the last line a run is allowed from the break the
    /// ellipsis is measured against, and that break includes the blank the line
    /// was broken at — so the closing line of a three-line announcement sat a
    /// space in from the two above it, and read as though it had been indented
    /// by hand.
    #[test]
    fn a_wrapped_line_does_not_begin_with_the_space_it_was_wrapped_at() {
        let mut font_system = shell_fonts();
        // Three lines exactly, which is what puts the third of them on the
        // layout's ellipsis path, and a body cut short, which is the other.
        for (content, size, bold, width, lines) in [
            (
                "File Downloaded. Actually not. I'm just testing a long title.",
                23.0,
                true,
                280.0,
                3,
            ),
            (
                "now  ·  report.pdf has been saved. I'm testing how long can the \
                 notification be in this shell in order to be readable.",
                20.0,
                false,
                340.0,
                3,
            ),
        ] {
            let mut pool = Vec::new();
            let mut run = label(content, width);
            run.size = size;
            run.bold = bold;
            run.lines = lines;
            shape_texts(&mut font_system, &mut pool, &[run]);

            let (_, buffer) = &pool[0];
            let runs: Vec<_> = buffer.layout_runs().collect();
            assert_eq!(runs.len(), lines as usize, "{content:?} at {width}");
            for line in &runs {
                let first = line.glyphs.first().expect("a line with letters on it");
                assert!(
                    !content[first.start..]
                        .chars()
                        .next()
                        .is_some_and(char::is_whitespace),
                    "line {:?} of {content:?} begins with a space",
                    &content[first.start..line.glyphs.last().unwrap().end]
                );
                // And every line starts where the one above it does, which is
                // what the reader actually sees.
                assert!(
                    first.x.abs() < 0.01,
                    "line {:?} is indented by {}",
                    &content[first.start..line.glyphs.last().unwrap().end],
                    first.x
                );
            }
        }
    }

    /// And a label that fits is left alone: nothing is trimmed from a run
    /// merely for being near the edge of its box.
    #[test]
    fn a_label_that_fits_keeps_all_of_itself() {
        let mut font_system = shell_fonts();
        let name = "Close Firefox";
        let mut pool = Vec::new();
        shape_texts(&mut font_system, &mut pool, &[label(name, 200.0)]);

        let (_, buffer) = &pool[0];
        let runs: Vec<_> = buffer.layout_runs().collect();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].glyphs.iter().map(|glyph| glyph.end).max(),
            Some(name.len())
        );
    }
}
