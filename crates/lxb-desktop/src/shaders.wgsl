// Shaders for the lattice shell.
//
// Coordinates arrive in physical pixels with the origin at the top-left; the
// vertex stages convert to clip space using the resolution uniform.

struct Globals {
    resolution: vec2<f32>,
    time: f32,
    // 0.0 sharp, 1.0 fully softened and dimmed (the guide's backdrop).
    blur: f32,
    // Pixel rect the background is drawn into, everything outside it left
    // transparent: the start screen shown as a miniature in its overview
    // card, and every size in between while it flies to or from the whole
    // display. All zeros fills the surface.
    window_rect: vec4<f32>,
    // x: corner radius in pixels, y: blur for the covers, z: cover count,
    // w: opacity of the background drawn into window_rect.
    params: vec4<f32>,
    // The wallpaper's palette, from the theme: gradient top and bottom, then
    // the pair the mood drifts towards.
    sky: array<vec4<f32>, 4>,
    // The active accent at its normal, soft and deep rungs. The wallpaper is
    // made from these rather than owning a second, unrelated highlight colour.
    accent: array<vec4<f32>, 3>,
    glow: vec4<f32>,
    // Rects whose square corners are painted over with the background that
    // sits behind them — the compositor's live window cards.
    covers: array<vec4<f32>, 6>,
    // The picture standing behind the shell: the layer being left, the layer
    // being arrived at, and how much of each is showing.
    hero: vec4<f32>,
    // Which material each half of the shell is drawn in — 0 for its own, 1 for
    // the plain one a slow machine asks for under Settings > Appearance > Theme.
    // x is the wallpaper and y is every mark the shell draws; they are separate
    // settings and either may be either way round. z is the shape of the user's
    // own picture where there is one — see `paper()`. w is one where the
    // sparkles the current carries are turned on, under Theme > Particles, and
    // nought where they are not, so a writer that has never heard of them
    // leaves them off and only one that asks the setting draws them.
    style: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;

// Steam's own pictures of the games under the cursors, one per layer, each
// with a chain of ever-smaller copies for the blur. Bound to both passes that
// draw the wallpaper, because the wallpaper is one function and it samples
// this.
@group(3) @binding(0) var scenery_texture: texture_2d_array<f32>;
@group(3) @binding(1) var scenery_sampler: sampler;

// The wallpaper the user chose, where they chose one: a picture of theirs, or
// the newest frame of a film of theirs, with the same chain of ever-smaller
// copies under it that everything else behind the glass has. Read only when
// `globals.style.x` says so — see `paper()` — and pointing at a single
// transparent texel on every machine that has never set one.
@group(3) @binding(2) var paper_texture: texture_2d<f32>;

// The shape a picture is kept at — `art::HERO_WIDTH` over `art::HERO_HEIGHT` —
// and the deepest rung of halvings it carries, one less than `HERO_LEVELS`.
// Both have to agree with Rust: a wrong shape squeezes every game's artwork,
// and a rung that was never uploaded samples whatever the layer held before.
const SCENERY_SHAPE: f32 = 1920.0 / 620.0;
const SCENERY_LEVELS: f32 = 4.0;

// How much of a game's picture reaches the screen.
//
// Not much of it, and this is the number that matters most in this whole
// feature. The shell's own wallpaper is deliberately dark: it is the ground
// white labels, a purple selection light and a room full of glass are all read
// against. Key art is the opposite — painted to be looked at on its own, at
// full brightness, with nothing on top of it. Half a bright sky behind the bar
// turns every pane of glass white and every label into grey text on cloud.
//
// Applied in linear light, so it is a gentler step than the number looks: a
// third of the light is around six tenths of the way up the sRGB ramp, which
// leaves a picture plainly legible as itself.
const SCENERY_LIGHT: f32 = 0.26;

// And how much further down the side of the screen the bar stands on.
//
// Every console that puts art behind a menu does this, because a picture is
// not evenly interesting: the half with the subject in it can be left alone,
// and the strip carrying the titles cannot. It eases out well before the
// middle so there is no edge to see — what is left is a picture that happens
// to be darker where the words are.
const SCENERY_SHADE: f32 = 0.5;
const SCENERY_SHADE_TO: f32 = 0.72;

// The surface is an sRGB target, so fragment outputs are interpreted as linear
// light. Theme colours have already been converted to linear light by Rust
// before they reach this module; all wallpaper arithmetic stays linear too.

// The one lamp the whole shell is lit by: up, to the left, and towards the
// viewer. Every rim highlight in the interface is this light seen in a
// different surface, which is what makes the panes and the wallpaper's silk
// current look like materials in the same room.
const KEY_LIGHT: vec3<f32> = vec3<f32>(-0.42, -0.66, 0.62);

// Half a turn. WGSL has no constant for it, and the wallpaper needs one: the
// ribbons are gathered together by a half-period whose ends land exactly on the
// two edges of the display.
const PI: f32 = 3.14159265;

// How wide a sheet of the current still is when the display sees it exactly
// edge-on, as a share of its own width. Rounded off just short of nothing: a
// sheet with no width at all is a crease rather than a fold, and its lighting
// degenerates on the singularity.
const BAND_FOLD: f32 = 0.020;

// How much the face of a sheet bows between its two lips, as a slope at the
// edge of the flat part. Water has no flat faces, and a face that really is
// flat carries one surface angle across its whole width, catches the sharp
// light everywhere at once, and leaves a straight-sided plateau on the ribbon.
const BAND_BOW: f32 = 0.50;

// The most of their two half-widths that can stand between two neighbouring
// ribbons of the current — the whole of what keeps the band one band. Under
// one, so two sheets always overlap whatever the twist has done to either of
// their widths, and far enough under it to cover the feathering of both
// silhouettes. `lxb-protocol`'s `the_three_ribbons_of_the_current_are_never_apart`
// is what holds this number to its promise.
const BAND_SHARE: f32 = 0.88;

// How far either side of a sheet's edge its silhouette is spread, in samples
// of whatever this wallpaper is being drawn into. A box filter a sample wide
// would spread it half a sample each way; this is wider because the fade is a
// smoothstep rather than a ramp, and a smoothstep does most of its travelling
// in the middle of the interval it is given. Kept in step with
// `lxb-protocol`'s `BAND_EDGE_SAMPLES`, which is where it was measured.
const BAND_EDGE_SAMPLES: f32 = 0.8;

// How far out a corner reaches, under the norm that shapes it.
//
// Two is the circle: the quarter-round arc every rounded rectangle has had
// since rounded rectangles existed, which meets the straight edge at a point
// where the curvature drops from all of it to none of it at once. Four is a
// squircle — the same corner with its bend spread out along the edges instead
// of stopping dead. On a shape whose radius is its whole half-width, which is
// to say on a circle, that difference is the whole difference between a disc
// and a rounded square.
fn corner_norm(v: vec2<f32>, power: f32) -> f32 {
    if (power <= 2.0) {
        return length(v);
    }
    return pow(pow(v.x, power) + pow(v.y, power), 1.0 / power);
}

// Signed distance to a rounded rectangle: negative inside, zero on the edge.
// One formula gives every rounded corner in the shell, at any size and at any
// sharpness of corner, without a texture per radius.
//
// Approximate above `power` of two — a p-norm reads slightly short across the
// diagonal, so the corner sits a little proud of where the true distance would
// put it. At a pixel of feather and a bevel of ten, that is not a difference
// anything can see.
fn rounded_box(point: vec2<f32>, half: vec2<f32>, radius: f32, power: f32) -> f32 {
    let r = min(radius, min(half.x, half.y));
    let q = abs(point) - half + vec2<f32>(r);
    return corner_norm(max(q, vec2<f32>(0.0)), power) + min(max(q.x, q.y), 0.0) - r;
}

// How much of the pixel at `px` falls inside the rounded rect `rect`, with a
// one-pixel feather so the curve does not stair-step.
fn rounded_coverage(px: vec2<f32>, rect: vec4<f32>, radius: f32) -> f32 {
    let half = rect.zw * 0.5;
    let d = rounded_box(px - (rect.xy + half), half, radius, 2.0);
    return 1.0 - smoothstep(-0.75, 0.75, d);
}

fn to_clip(position: vec2<f32>) -> vec4<f32> {
    let ndc = vec2<f32>(
        position.x / globals.resolution.x * 2.0 - 1.0,
        1.0 - position.y / globals.resolution.y * 2.0,
    );
    return vec4<f32>(ndc, 0.0, 1.0);
}

// ---------------------------------------------------------------------------
// Background: a single oversized triangle covering the screen.
// ---------------------------------------------------------------------------

struct BackgroundOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_background(@builtin(vertex_index) index: u32) -> BackgroundOut {
    // Full-screen triangle; the third vertex lies outside the viewport.
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(3.0, 1.0),
    );
    let p = positions[index];

    var out: BackgroundOut;
    out.clip = vec4<f32>(p, 0.0, 1.0);
    // Map clip space to 0..1 with y running downwards.
    out.uv = vec2<f32>((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
    return out;
}

// A huge, feathered pool of light. Its radii are deliberately comparable to a
// display rather than to an object on it: as the centres move, the *field*
// changes, without anything looking like a particle travelling across it.
fn ambient_field(p: vec2<f32>, center: vec2<f32>, radius: vec2<f32>) -> f32 {
    let q = (p - center) / radius;
    return exp(-dot(q, q) * 1.65);
}

// The picture behind the shell at `uv`, and how much of it there is.
//
// Cropped to fill rather than squeezed to fit: a hero is wider than any
// display, and the shape it was painted in is the shape it has to keep. What
// is cut is the sides, which is what Valve's own guidance is written for —
// everything that shows one of these crops it.
//
// Two layers, because the picture changes as the cursor moves and one must not
// blink out to make room for the next. They are added by weight and divided by
// the weight they carry, so a crossfade never dips through the wallpaper
// underneath on its way across.
//
// A layer's own transparency counts as well as its weight. Every picture Steam
// publishes is opaque, so for a game this is exactly the weighted average it
// was before; what it is for is the other kind of picture that can stand here —
// one of the user's own files, which may be a drawing on nothing at all. That
// one is shown as what it is, with the shell's wallpaper where the file has no
// pixels, rather than as a black band the size of the display.
fn scenery(uv: vec2<f32>, aspect: f32, lod: f32) -> vec4<f32> {
    let leaving = globals.hero.z;
    let arriving = globals.hero.w;
    let total = leaving + arriving;
    if (total <= 0.0) {
        return vec4<f32>(0.0);
    }

    var window = vec2<f32>(1.0, 1.0);
    if (aspect < SCENERY_SHAPE) {
        window.x = aspect / SCENERY_SHAPE;
    } else {
        window.y = SCENERY_SHAPE / aspect;
    }
    let cropped = (uv - vec2<f32>(0.5)) * window + vec2<f32>(0.5);

    var color = vec3<f32>(0.0);
    var covered = 0.0;
    if (leaving > 0.0) {
        let layer = textureSampleLevel(
            scenery_texture, scenery_sampler, cropped, i32(globals.hero.x), lod);
        color += layer.rgb * layer.a * leaving;
        covered += layer.a * leaving;
    }
    if (arriving > 0.0) {
        let layer = textureSampleLevel(
            scenery_texture, scenery_sampler, cropped, i32(globals.hero.y), lod);
        color += layer.rgb * layer.a * arriving;
        covered += layer.a * arriving;
    }
    if (covered <= 0.0) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(color / covered, min(total, 1.0) * covered / total);
}

// The user's own wallpaper at `uv`, cropped to fill the display.
//
// Cropped rather than squeezed, exactly as a game's key art is and for the same
// reason: a picture has a shape of its own, and the one thing that must not
// happen to somebody's photograph is being stretched into the shape of their
// monitor. What is given up is the sides of a picture wider than the screen, or
// the top and bottom of a taller one.
//
// `lod` is the rung of the blur chain frost and the guide ask for, which is why
// the copies exist at all: the analytic scene answers "softer, please" by
// drawing itself wider and dimmer, and a photograph can only answer it by
// having smaller copies to be read from.
fn paper(uv: vec2<f32>, aspect: f32, lod: f32) -> vec3<f32> {
    let shape = globals.style.z;
    var window = vec2<f32>(1.0, 1.0);
    if (aspect < shape) {
        window.x = aspect / shape;
    } else {
        window.y = shape / aspect;
    }
    let cropped = (uv - vec2<f32>(0.5)) * window + vec2<f32>(0.5);
    let sampled = textureSampleLevel(paper_texture, scenery_sampler, cropped, lod);
    // A drawing on nothing at all — an SVG, a PNG with a transparent
    // background — shows the shell's own dark ground through it rather than
    // black, which is the same bargain `scenery` strikes with the same kind of
    // file. The ground is the theme's own gradient at its darkest, so a picture
    // with a hole in it still sits in this shell rather than on a void.
    let ground = mix(globals.sky[0].rgb, globals.sky[1].rgb, uv.y) * 0.42;
    return mix(ground, sampled.rgb, sampled.a);
}

// Where the current runs at `u` across the display: how much of the band's
// spread survives there, the height of the one curve it is stacked along, and
// how steeply that curve climbs, in the same physical-screen units as y.
//
// One curve with two things riding it — the band of water, stacked along it,
// and the sparkles it sheds — so it is worked out once, here,
// rather than by each of them, where one copy could drift from the other.
struct Spine {
    gather: f32,
    height: f32,
    slope: f32,
};

// Where the spine rests, down the display, when nothing is swinging it: below
// the cross point, in the lane the whole current runs through.
const SPINE_REST: f32 = 0.62;

fn spine_at(u: f32, t: f32, aspect: f32) -> Spine {
    // Everything the band is made of is gathered in towards its lane at both
    // ends of the display: the ribbons come off the left edge close together,
    // open apart across the middle, and close again on the way off the right.
    // Never all the way to nothing, or the band would leave as one line.
    let gather = 0.28 + 0.72 * sin(PI * u);
    let gather_slope = 0.72 * PI * cos(PI * u);

    // The spine: the one curve the band is stacked along, and so the only one
    // whose slope has to be measured.
    let spine_a = u * 6.8 + t * 0.56;
    let spine_b = u * 3.4 - t * 0.39 + 0.8;
    let swing = sin(spine_a) * 0.055 + sin(spine_b) * 0.085;
    let swing_slope = cos(spine_a) * 0.055 * 6.8 + cos(spine_b) * 0.085 * 3.4;

    var spine: Spine;
    spine.gather = gather;
    spine.height = SPINE_REST + swing * gather;
    // Measure across the curve rather than vertically. Without this correction
    // a steep section grows visibly thicker than a flat one. In the same
    // physical-screen units as y, so wide outputs do not over-correct either
    // the width or the light angle. The gathering is part of the curve, and so
    // is its slope.
    spine.slope = (swing_slope * gather + swing * gather_slope) / aspect;
    return spine;
}

// The current as the shell's own material: one band of water, three ribbons
// thick, drifting through a broad lane below the cross point.
//
// Takes the scene as it stands and hands it back with the band drawn into it,
// because water is a body rather than a glow — it takes light out of what
// stands behind it before putting any of its own back.
fn water(
    into: vec3<f32>,
    uv: vec2<f32>,
    aspect: f32,
    t: f32,
    soften: f32,
    footprint: vec2<f32>,
    spine: Spine,
) -> vec3<f32> {
    var color = into;
    // The current: one band of water, three ribbons thick, drifting
    // through a broad lane below the cross point.
    //
    // Each ribbon is a sheet of water rather than a drawn line, and it is made
    // of the same material as everything else in this shell: the cross-section
    // is the bead `glyph_material` builds — a flat face with a quarter-round
    // lip rolled over at both edges, from the same `bevel_rise` and
    // `bevel_slope` — carried along a travelling curve. One angle rolls each
    // sheet about its own travel, which is what makes it widen and narrow,
    // hand its highlight from one edge to the other, and throw a shadow on the
    // wallpaper it stands in front of. A second axis of much smaller waves
    // runs along its length, and that is the one that makes it water: a sheet
    // that can only bend across its width has a highlight running its whole
    // length in one unbroken line — a wire of chrome — because somewhere in a
    // smooth roll from flat face to steep lip there is always an angle that
    // catches the lamp.
    //
    // The three are stacked along one spine, and they cannot come apart. Each
    // outer ribbon is pushed off the middle one by a *share* of the two
    // half-widths that meet there rather than by a length of its own: a share
    // that passes through zero, so they cross, and never reaches one, so the
    // sheets always overlap. Nothing the twist or the drift does can open a gap
    // between them, because the space between them is not a distance — it is a
    // fraction of a width that shrinks when they do.
    //
    // Nothing is refracted from behind: a sheet this thin in front of a field
    // this smooth displaces nothing the eye could see, and what says water
    // here is the geometry of the surface.
    //
    // How much of a half-width the rolled lip takes. The rest is flat face,
    // and a ribbon narrower than twice this is all lip — which is correct, and
    // is what keeps the finer strands beads of water rather than flat rails.
    let lip = 0.45;
    // Blurred, the sheet is drawn wide and dim like everything else here, and
    // its sharp light is nearly all given up: a glinting highlight behind a
    // frosted pane is the one thing that would say the backdrop is a picture
    // rather than the room.
    let ribbon_gloss = mix(1.0, 0.10, soften);
    let ribbon_wave = mix(1.0, 0.25, soften);
    // The lamp the whole shell shares, and the half vector between it and an
    // eye looking straight into the display.
    let key = normalize(KEY_LIGHT);
    let half_vector = normalize(key + vec3<f32>(0.0, 0.0, 1.0));

    // The curve the band is stacked along, and how much of its spread
    // survives here. See `spine_at`.
    let gather = spine.gather;
    let slope = spine.slope;
    let across = normalize(vec2<f32>(slope, -1.0));
    // And along the band, in those same units.
    let along = vec2<f32>(-across.y, across.x);
    let band = (uv.y - spine.height) / sqrt(1.0 + slope * slope);
    // How far across the band one sample reaches, in those same units: the
    // sample's own two sides, each as much of them as points across the curve.
    // Nothing else in this picture needs it — every other term here is a field
    // that changes slowly enough to be read one point at a time — but the
    // sheets have edges, and an edge drawn from a single point either lands in
    // a sample or does not. It is not always a display pixel: the same
    // wallpaper is drawn into a card's miniature, where it is the whole picture
    // squeezed into a few hundred of them.
    let sample = abs(across.x * footprint.x * aspect) + abs(across.y * footprint.y);

    // Every ribbon's twist and width before any of them is drawn, because
    // where one of them stands depends on how wide the one beside it is.
    var tilt = array<f32, 3>(0.0, 0.0, 0.0);
    var width = array<f32, 3>(0.0, 0.0, 0.0);
    for (var i = 0; i < 3; i = i + 1) {
        let fi = f32(i);
        let x = uv.x * (2.0 + fi * 0.6);
        // How far this sheet has turned about its own travel. Three opposing
        // clocks whose periods do not divide one another, so the twist never
        // settles into a pattern that repeats down the ribbon, and deep enough
        // that it carries the sheet past edge-on — which is what a wrapping
        // ribbon does, and where it pinches.
        let turning = sin(x * 2.10 - t * (0.23 + fi * 0.05) + fi * 1.9) * 0.95
            + sin(x * 1.25 + t * 0.15 + fi * 2.7) * 0.55
            + sin(x * 4.30 - t * 0.35 + fi * 0.7) * 0.22;
        // Squared, keeping its sign: a ribbon lies open for a long run and then
        // turns through its twist quickly, rather than rolling evenly the whole
        // way along like a screw.
        tilt[i] = (turning * abs(turning) * 0.62
            + sin(x * 7.0 + t * 0.9 + fi) * 0.08) * ribbon_wave;
        // How much of the sheet's own width the display sees. Edge-on it is
        // nearly none of it: the ribbon pinches to a fold there and opens out
        // again on the far side, with its lit edge handed over to the other
        // side of itself.
        let broad = sqrt(cos(tilt[i]) * cos(tilt[i]) + BAND_FOLD);
        width[i] = (0.0640 - 0.0110 * fi) * broad * mix(1.0, 1.5, soften);
    }

    // Where each ribbon stands across the band. The middle one *is* the band;
    // the other two are pushed off it by their share, and `BAND_SHARE` — under
    // one — is the whole of what keeps the three of them joined.
    let lift = BAND_SHARE * (0.32 + 0.68 * sin(uv.x * 4.3 - t * 0.37));
    let drop = BAND_SHARE * (0.32 + 0.68 * sin(uv.x * 3.1 + t * 0.29 + 2.2));
    let offset = array<f32, 3>(
        -(width[0] + width[1]) * lift * gather,
        0.0,
        (width[1] + width[2]) * drop * gather,
    );

    for (var i = 0; i < 3; i = i + 1) {
        let fi = f32(i);
        let x = uv.x * (2.0 + fi * 0.6);
        let d = band - offset[i];
        let sheet_width = width[i];

        // The small water on top of the twist, as the slope of waves
        // travelling along the ribbon's own length.
        let along_wave = (cos(x * 9.0 - t * 1.1 + fi * 2.0) * 0.34
            + cos(x * 23.0 + t * 1.9 + fi * 1.3) * 0.14) * ribbon_wave;

        let reach = abs(d / sheet_width);
        // Where across the ribbon this pixel is, signed, for the light that
        // travels through the sheet at an angle.
        let s_across = clamp(d / sheet_width, -1.0, 1.0);
        // The silhouette, feathered only in its last few percent: water holds
        // its own edge, and the rim light needs a surface to sit on. That is
        // the fade as authored, and what a display's own frame very nearly
        // draws.
        //
        // Widened by a sample either side of the edge, which is what makes the
        // same band bear being drawn small — into a card's miniature, or into
        // the compositor's bridge frame, a fifth of a display across. This is
        // the whole of the anti-aliasing the band gets and all it needs: the
        // silhouette is the only place this picture stops being smooth, and
        // everything sharp about a sheet — the lip, the glint, the dispersion —
        // is carried by `cover` and goes soft with it.
        //
        // Either side of the edge, rather than inwards from it: a fade that
        // only ate into the sheet would thin the water as the picture got
        // smaller, and where a ribbon is pinched nearly edge-on it would take
        // most of it. Spread symmetrically, this is close to what a
        // sample-wide box filter of the same edge lands on, which is the
        // picture more samples would converge to.
        let spread = BAND_EDGE_SAMPLES * sample / sheet_width;
        let cover = 1.0
            - smoothstep(mix(0.94, 0.40, soften) - spread, 1.0 + spread, reach);
        // Where in the rolled-over lip this pixel is: 0 at the outer edge, 1
        // where the flat face begins.
        let inset = clamp((1.0 - reach) / lip, 0.0, 1.0);
        let rise = bevel_rise(inset);

        // The cross-section's own angle: the rolled lip, turned with the whole
        // sheet. Kept as a sine and a cosine rather than as radians, so the
        // steep lip costs a rotation instead of an arctangent.
        // The lip, and the bow of the face inside it. A sheet of water is never
        // flat: without the bow the face's whole width has one surface angle,
        // so it satisfies the sharp light all at once and flashes as a plateau
        // with a straight edge down each side of it, which reads as a rectangle
        // laid on the ribbon. Bowed, the same light is a band running along the
        // sheet, and the waves along its length break that band up.
        let wall = normalize(vec2<f32>(
            -sign(d) * bevel_slope(inset) - s_across * BAND_BOW,
            1.0,
        ));
        let turn = sin(tilt[i]);
        let level = cos(tilt[i]);
        let face = vec2<f32>(
            wall.x * level + wall.y * turn,
            wall.y * level - wall.x * turn,
        );
        // The same water behind less display is brighter for it, up to a
        // ceiling — or the pinch itself would be the brightest thing on screen.
        let broad = sqrt(level * level + BAND_FOLD);
        let fold = min(1.0 / broad, 2.2);
        // The surface itself: that angle across the ribbon, the wave slope
        // along it, and what is left of it facing the display.
        let surface = normalize(vec3<f32>(
            across * face.x + along * along_wave * face.y,
            face.y,
        ));

        let facing = clamp(dot(surface, key), 0.0, 1.0);
        let fresnel = 0.04 + 0.96 * pow(1.0 - clamp(surface.z, 0.0, 1.0), 5.0);
        let glint = pow(max(dot(surface, half_vector), 0.0), 42.0);
        // The room the sheet hands back at a grazing angle. Only the
        // brightness of it is kept, the way a glyph keeps only the brightness
        // of the sky it reflects: the colour belongs to the accent.
        let mirrored = reflect(vec3<f32>(0.0, 0.0, -1.0), surface);
        let room = 0.42 + 0.73 * (0.5 - 0.5 * dot(mirrored, key));
        // Only the sharp light travels quickly, so the sheet glistens without
        // the whole ribbon pulsing.
        let travelling = 0.60 + 0.40
            * sin(x * 3.1 - t * (0.9 + fi * 0.25) + fi);
        // What the light gathers on its way through the sheet: bright arcs
        // lying across the body wherever a ripple above them is focusing. They
        // ride the finer of the two waves that bend the surface, rather than a
        // pattern of their own — a second, unrelated period reads as hatching
        // rather than as water — they lean because the light crosses the sheet
        // at an angle on its way through, and they come in patches, because
        // arcs all the way along a ribbon read as corrugation. The patch is a
        // squared half-wave rather than a clipped one: clipping a sine leaves a
        // kink, and a kink in something that only varies along the ribbon is a
        // straight cut across it — which reads as a rectangle pasted on the
        // water.
        let focusing = 0.5 + 0.5 * sin(x * 2.3 - t * 0.5 + fi);
        let gathered = pow(max(sin(x * 23.0 + t * 1.9 + fi * 1.3
            + s_across * 3.4), 0.0), 5.0)
            * (0.15 + 0.85 * focusing * focusing);

        let depth = 1.0 - fi * 0.26;
        let skirt = exp(-d * d * mix(90.0, 45.0, soften));
        // The shadow the sheet throws on the wallpaper, down-light of itself.
        // The one thing that says the band is in front of the scene rather
        // than mixed into it.
        let shadow_d = d - sheet_width - 0.010;
        let shadow = exp(-shadow_d * shadow_d * mix(1400.0, 500.0, soften))
            * (1.0 - cover);
        // And the line its curve gathers on the far side of that shadow, where
        // the light it let through comes back together.
        let caustic_d = d - sheet_width - 0.0040;
        let caustic = exp(-caustic_d * caustic_d * mix(30000.0, 2000.0, soften))
            * broad;

        let haze_tint = mix(globals.accent[0].rgb, globals.accent[2].rgb, 0.46);
        let body_tint = mix(globals.accent[0].rgb, globals.accent[2].rgb,
                            0.24 + fi * 0.05 + 0.36 * rise);
        // A thick edge splits what passes through it into colour: warm above
        // the lip, cold below it. Small, because it is a cue and not a prism.
        let split = GLASS_DISPERSION * (1.0 - inset) * (1.0 - inset)
            * cover * depth * ribbon_gloss * -sign(d);

        let haze_strength = mix(1.0, 0.40, soften) * depth;
        let body_strength = mix(1.0, 0.55, soften) * depth;
        let water = cover * fold * body_strength;
        // Water is a body: it takes light out of what stands behind it and
        // shades what stands beside it, before any of its own is added.
        color *= 1.0 - cover * (0.05 + 0.11 * rise) * body_strength;
        color *= 1.0 - shadow * 0.20 * body_strength;
        color += haze_tint * skirt * 0.007 * haze_strength;
        color += body_tint * water * (0.005 + 0.012 * rise + 0.028 * facing);
        color += globals.accent[1].rgb * fresnel * room * water * 0.100
            * mix(1.0, 0.45, soften);
        color += globals.accent[1].rgb * glint * water * 0.120 * travelling
            * ribbon_gloss;
        color += globals.accent[1].rgb * gathered * cover * rise * 0.011
            * depth * ribbon_gloss;
        color += globals.accent[1].rgb * caustic * 0.011 * depth * ribbon_gloss;
        color *= vec3<f32>(1.0 + split, 1.0, 1.0 - split);
    }
    return color;
}

// The current as the plainer material: three fine glass-silk ribbons moving
// together through the same lane.
//
// What this shell drew before the band, kept for the Simple theme rather than
// deleted. A machine that cannot afford the water still gets a current, and
// this is the one that was tuned to sit under the bar without competing with
// it. It adds and never subtracts, which is half of why it is cheap: no pixel
// behind it is read back and dimmed.
fn silk(into: vec3<f32>, uv: vec2<f32>, aspect: f32, t: f32, soften: f32) -> vec3<f32> {
    var color = into;
    // The current: three fine glass-silk ribbons moving together through
    // a broad lane below the cross point. Each ribbon has a stable translucent
    // body, a deep lower fold and an accent-soft bevel catching the shell's
    // upper-left lamp. Only that highlight carries the quicker travelling
    // sheen, so the material glistens without the whole line pulsing like neon.
    for (var i = 0; i < 3; i = i + 1) {
        let fi = f32(i);
        let speed = 0.42 + fi * 0.14;
        let lane = 0.62 + (fi - 1.0) * 0.050;
        let x_scale = 2.0 + fi * 0.6;
        let x = uv.x * x_scale;

        let phase_a = x * 2.6 + t * speed + fi * 2.1;
        let phase_b = x * 1.3 - t * speed * 0.7 + fi * 0.8;
        let center = lane + sin(phase_a) * 0.055 + sin(phase_b) * 0.085;

        // Measure across the curve rather than vertically. Without this
        // correction a steep section grows visibly thicker than a flat one.
        // Convert x to the same physical-screen units as y first, so wide
        // outputs do not over-correct either the width or the light angle.
        let uv_slope = (cos(phase_a) * 0.055 * 2.6
            + cos(phase_b) * 0.085 * 1.3) * x_scale;
        let slope = uv_slope / aspect;
        let d = (uv.y - center) / sqrt(1.0 + slope * slope);

        let skirt = exp(-d * d * mix(320.0, 110.0, soften));
        let body = exp(-d * d * mix(5200.0, 680.0, soften));
        let bevel_d = d + mix(0.0045, 0.012, soften);
        let bevel = exp(-bevel_d * bevel_d * mix(20000.0, 1000.0, soften));
        let crest_d = d + mix(0.0065, 0.014, soften);
        let crest = exp(-crest_d * crest_d * mix(70000.0, 1400.0, soften));
        let fold_d = d - mix(0.008, 0.016, soften);
        let lower_fold = exp(-fold_d * fold_d * mix(9500.0, 900.0, soften));

        // A bend facing the shared lamp catches more of its highlight. The
        // separate travelling term is restrained to the glossy layers.
        let upper_normal = normalize(vec2<f32>(slope, -1.0));
        let lamp_facing = max(dot(upper_normal, normalize(KEY_LIGHT.xy)), 0.0);
        let key_glint = 0.52 + 0.48 * pow(lamp_facing, 4.0);
        let travelling = 0.64 + 0.36
            * sin(x * 3.1 - t * (0.9 + fi * 0.25) + fi);

        let depth = 1.0 - fi * 0.18;
        let haze_strength = mix(1.0, 0.40, soften) * depth;
        let gloss_strength = mix(1.0, 0.10, soften) * depth;
        let haze_tint = mix(globals.accent[0].rgb, globals.accent[2].rgb, 0.46);
        let body_tint = mix(globals.accent[0].rgb, globals.accent[2].rgb,
                            0.28 + fi * 0.04);

        color += haze_tint * skirt * 0.020 * haze_strength;
        color += body_tint * body * 0.040 * haze_strength;
        color += globals.accent[2].rgb * lower_fold * 0.010 * haze_strength;
        color += globals.accent[1].rgb
            * (bevel * 0.022 * (0.82 + 0.18 * travelling)
                + crest * 0.010 * travelling * key_glint)
            * gloss_strength;
    }
    return color;
}

// Where the middle of the plainer current runs: the curve `silk` draws its
// middle ribbon along, as a spine. What the sparkles are shed from when the
// wallpaper is drawn in that material — they come out of the ribbon on the
// screen, not out of the band of water that is not there.
fn silk_spine(u: f32, t: f32, aspect: f32) -> Spine {
    let speed = 0.56;
    let x_scale = 2.6;
    let x = u * x_scale;
    let phase_a = x * 2.6 + t * speed + 2.1;
    let phase_b = x * 1.3 - t * speed * 0.7 + 0.8;
    var spine: Spine;
    spine.gather = 1.0;
    spine.height = SPINE_REST + sin(phase_a) * 0.055 + sin(phase_b) * 0.085;
    spine.slope = (cos(phase_a) * 0.055 * 2.6 + cos(phase_b) * 0.085 * 1.3) * x_scale / aspect;
    return spine;
}

// How far out from the ribbon a sparkle can still be seen, in display heights,
// where the band is widest. Narrower where the band gathers towards the edges
// of the display, as the ribbons themselves do.
const SPARKLE_LANE: f32 = 0.28;

// How far from the middle of the ribbon a sparkle may first appear. Close:
// every sparkle starts in the heart of the band and is on its way out of it
// from then on, so the middle is where they are always coming from and never
// empty.
const SPARKLE_BIRTH: f32 = 0.03;

// The most a row of cells is ever squeezed on the screen, as a share of its
// height in the grid. Each depth's hold squeezes the rows nearest the ribbon as
// it swings into them and its push stretches them (see `SparkleLayer`), and
// between the two a row is never shorter than this — which is what the cells'
// height is measured against, so that no glow reaches past the rows a point
// asks.
const SPARKLE_SQUEEZE: f32 = 0.85;

// Sparkles shed below the ribbon sink at this share of the pace the ones above
// it rise. They leave in both directions, but slowly downwards: specks falling
// through the picture read as snow, which is what this scene drew once and was
// taken out for.
const SPARKLE_SINK: f32 = 0.6;

// The steepest the spine is taken to climb where a sparkle is placed against
// it. No display wider than it is tall comes near it; a monitor turned on its
// side can ask for more, and there a sparkle on the steepest stretches is
// placed a little off the true curve rather than cut off by its cell's edge.
const SPARKLE_STEEPEST: f32 = 0.95;

// One depth of sparkles.
//
// `cell` is how long a cell is along the display and how tall across the
// spine, in display heights; each holds at most one sparkle. `drift` is how
// fast the rows drift along the display and `rise` how fast they move out of
// the ribbon, in display heights a second, before each row's own share of
// either. `density` is how many cells have a sparkle in them. `core` is the
// radius of the largest core and `reach` the furthest the largest glow goes.
// `travel` is the shortest and the longest way a sparkle goes before it has
// faded out. `smallest` is how small the smallest of them is, as a share of
// the largest, and `brightness` how bright their cores and their glows are.
//
// `push` is how hard the ribbon throws them off: a sparkle leaves it
// `1 + push.x / push.y` times faster than it later drifts, and the push is
// spent over about `push.y` of its own drift — so the ribbon throws them and
// they coast, rather than seeping out of it at an even pace nobody could see
// against its swing. `hold` is how much of the ribbon's swing a sparkle still
// moves with once it is far from it, and over how far from it the hold
// slackens: at the ribbon it moves with all of it — the band is shoving it —
// and a sparkle thrown clear with only `hold.x`, so the ribbon swings into what
// it has shed and drives it on, rather than carrying the whole cloud about as
// one sheet. Both are stretches of the distance from the spine that
// `lxb-protocol`'s `the_hold_and_the_push_never_squeeze_a_row_past_its_share`
// holds to `SPARKLE_SQUEEZE`.
struct SparkleLayer {
    seed: u32,
    cell: vec2<f32>,
    drift: f32,
    rise: f32,
    density: f32,
    core: f32,
    reach: f32,
    travel: vec2<f32>,
    smallest: f32,
    brightness: vec2<f32>,
    push: vec2<f32>,
    hold: vec2<f32>,
};

// The fine glitter the ribbon is full of: many and small, pushed gently and
// held, so the band stays full of it.
const SPARKLE_DUST: SparkleLayer = SparkleLayer(
    0u,
    vec2<f32>(0.020, 0.023),
    0.018,
    0.016,
    0.90,
    0.0020,
    0.0070,
    vec2<f32>(0.05, 0.14),
    0.60,
    vec2<f32>(1.00, 0.15),
    vec2<f32>(0.03, 0.03),
    vec2<f32>(0.45, 0.15),
);

// And the few that carry a glow round them, which the ribbon throws hard and
// lets go of.
const SPARKLE_GLINTS: SparkleLayer = SparkleLayer(
    1013904223u,
    vec2<f32>(0.075, 0.085),
    0.022,
    0.018,
    0.60,
    0.0048,
    0.026,
    vec2<f32>(0.07, 0.18),
    0.35,
    vec2<f32>(0.85, 0.22),
    vec2<f32>(0.06, 0.02),
    vec2<f32>(0.35, 0.15),
);

// A number that looks nothing like the one it was made from, and is the same
// number on every machine: the permutation PCG finishes its output with. Whole
// numbers rather than the sine-of-a-large-number every shader reaches for,
// because the compositor draws this same scene on the CPU before the shell is
// up, and a sine that far out is a different number on every implementation.
fn sparkle_hash(value: u32) -> u32 {
    let state = value * 747796405u + 2891336453u;
    let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

// That number as a share, 0 up to but never 1. Twenty-four bits of it, which
// is every bit an f32 can hold exactly.
fn sparkle_unit(value: u32) -> f32 {
    return f32(value >> 8u) / 16777216.0;
}

// A share out of some of its bits: `mask` of them, from `shift` up. One hash
// holds a sparkle's position both ways, and one more everything else about it,
// which is two of these per sparkle rather than one per question asked of it.
fn sparkle_bits(value: u32, shift: u32, mask: u32) -> f32 {
    return f32((value >> shift) & mask) / f32(mask + 1u);
}

// How far from the spine a sparkle `drift` of the way out has really come,
// pushed as `push` says. Odd, so behind the spine is the same stretch
// mirrored, and never shallower than one to one.
fn sparkle_pushed(drift: f32, push: vec2<f32>) -> f32 {
    return drift + push.x * drift / (push.y + abs(drift));
}

// And back: how far out in the grid a point `out` from the spine is. The root
// of the quadratic `sparkle_pushed` comes to, in whichever of its two forms
// does not cancel itself away.
fn sparkle_unpushed(out: f32, push: vec2<f32>) -> f32 {
    let d = abs(out);
    let b = push.y + push.x - d;
    let root = sqrt(b * b + 4.0 * d * push.y);
    let drift = select(0.5 * (root - b), 2.0 * d * push.y / (b + root), b > 0.0);
    return sign(out) * drift;
}

// How much of the ribbon's swing a sparkle `out` from it moves with. All of it
// at the ribbon and behind it, easing to `hold.x` far from it.
fn sparkle_hold(out: f32, hold: vec2<f32>) -> f32 {
    let a = max(out, 0.0);
    return (hold.y + hold.x * a) / (hold.y + a);
}

// How far out from the ribbon a point is, on one side of it, where the ribbon
// has swung `held` towards that side and the point stands `offset` out from
// where the ribbon rests: the one distance `out` for which `out` plus the part
// of the swing a sparkle there moves with lands on the point. The root of the
// quadratic that comes to, in whichever of its two forms does not cancel
// itself away; behind the ribbon, where the hold is whole, a straight line.
fn sparkle_unheld(offset: f32, held: f32, hold: vec2<f32>) -> f32 {
    if (offset <= held) {
        return offset - held;
    }
    let b = hold.y + held * hold.x - offset;
    let c = 4.0 * hold.y * (offset - held);
    let root = sqrt(b * b + c);
    return select(0.5 * (root - b), 2.0 * hold.y * (offset - held) / (b + root), b > 0.0);
}

// A soft round bump that is exactly nothing from `x = 1` outwards — the shape
// of a Gaussian without its tail, and it is the missing tail that lets a cell
// be sure no sparkle but its own and its three nearest neighbours' can reach it.
fn sparkle_bump(x: f32) -> f32 {
    let q = max(1.0 - x * x, 0.0);
    return q * q * q;
}

// The sparkles shed on one side of the ribbon — `side` is one below it and
// minus one above it — at a point `offset` below where the ribbon rests, with
// the ribbon swung `swing` below its rest: their cores and their glows, before
// either is coloured.
//
// The cells are laid out across the ribbon in rows that move out of it, so
// every sparkle's distance from the spine grows at the row's pace — and each
// one is dark until it has come out as far as its own birth, somewhere inside
// the body of the band, so it appears *in* the ribbon, leaves it, and fades.
//
// No sparkle reaches further than `reach`, and no cell is shorter than twice
// that however steeply the spine climbs up to `SPARKLE_STEEPEST`, so the four
// cells nearest a point are the only ones on its side that can light it — and
// of those, only the ones whose edge is within that reach are asked at all.
// `lxb-protocol`'s `no_sparkle_is_cut_off_by_the_edge_of_its_cell` is what
// holds that promise, and it is what keeps this cheap.
fn sparkle_half(
    layer: SparkleLayer,
    side: f32,
    offset: f32,
    swing: f32,
    along: f32,
    slope: f32,
    lane: f32,
    t: f32,
    sample: f32,
    soften: f32,
) -> vec2<f32> {
    var light = vec2<f32>(0.0);
    let reach_across = layer.reach * sqrt(1.0 + slope * slope);
    let rise = layer.rise * select(SPARKLE_SINK, 1.0, side < 0.0);
    let half_seed = layer.seed ^ select(0u, 0x9e3779b9u, side < 0.0);
    let out_here = sparkle_unheld(side * offset, side * swing, layer.hold);
    let row_at = (sparkle_unpushed(out_here, layer.push) - rise * t) / layer.cell.y;
    let row_here = floor(row_at);
    let row_in = row_at - row_here;
    let row_side = select(-1.0, 1.0, row_in >= 0.5);
    // The row beyond is only worth asking when something standing on its near
    // edge could reach this far — which it does from further off the steeper
    // the spine, since the rows lie along it, and the more the row between is
    // squeezed.
    let row_gap = select(row_in, 1.0 - row_in, row_in >= 0.5) * layer.cell.y;
    let rows = select(1, 2, row_gap * SPARKLE_SQUEEZE < reach_across);
    for (var r = 0; r < rows; r = r + 1) {
        let row = row_here + f32(r) * row_side;
        let row_seed = sparkle_hash(bitcast<u32>(i32(row)) + half_seed);
        let drifted = along - layer.drift * (0.6 + 0.8 * sparkle_unit(row_seed)) * t;
        let column_at = drifted / layer.cell.x;
        let column_here = floor(column_at);
        let column_in = column_at - column_here;
        let column_side = select(-1.0, 1.0, column_in >= 0.5);
        let column_gap = select(column_in, 1.0 - column_in, column_in >= 0.5) * layer.cell.x;
        let columns = select(1, 2, column_gap < layer.reach);
        for (var c = 0; c < columns; c = c + 1) {
            let column = column_here + f32(c) * column_side;
            let cell_seed = sparkle_hash(row_seed + bitcast<u32>(i32(column)));
            if (sparkle_unit(cell_seed) >= layer.density) {
                continue;
            }

            // Where it floats: somewhere in its cell, and about that place with
            // a slow sway to and fro and a slow drift up and down of its own,
            // on clocks that are not its row's — as far as either can carry
            // it, which is never out of the cell. Asked in the order that lets
            // a sparkle out of reach be passed over soonest — past its reach it
            // lights nothing at all.
            let shape = sparkle_hash(cell_seed);
            let look = sparkle_hash(shape);
            let phase = 2.0 * PI * sparkle_bits(look, 24u, 0xffu);
            let sway = sin(t * (0.12 + 0.20 * sparkle_bits(cell_seed, 0u, 0xffu)) + 2.0 * phase + 1.0)
                * 0.18;
            let d_along = (column + 0.5 + 0.60 * (sparkle_bits(shape, 0u, 0xffffu) - 0.5) + sway)
                * layer.cell.x - drifted;
            if (abs(d_along) >= layer.reach) {
                continue;
            }
            let strength = sparkle_bits(look, 0u, 0xffu);
            let grain = sparkle_bits(look, 8u, 0xffu);
            let pace = sparkle_bits(look, 16u, 0xffu);
            let wander = sin(t * (0.15 + 0.25 * pace) + phase) * 0.22;
            // How far out of the ribbon it has come by now, pushed.
            let out = sparkle_pushed((row + 0.5 + 0.56 * (sparkle_bits(shape, 16u, 0xffffu) - 0.5)
                + wander) * layer.cell.y + rise * t, layer.push);
            // Where it is on the screen: that far out on its side, plus the part
            // of the ribbon's swing it still moves with — and the ribbon climbs,
            // so that part climbs with it, `slope` higher for every step along.
            let hold = sparkle_hold(out, layer.hold);
            let d_across = hold * swing + side * out - offset + hold * slope * d_along;
            let apart = sqrt(d_along * d_along + d_across * d_across);
            // Mostly small and faint, now and then large and bright.
            let size = mix(layer.smallest, 1.0, grain * grain);
            let reach = layer.reach * size;
            if (apart >= reach) {
                continue;
            }

            // Its journey: dark until it has come out as far as its birth,
            // close to the middle of the band, then lit quickly — and from
            // there on more and more see-through the further it goes, until it
            // is gone — and behind it the next row is already coming out of the
            // middle. It twinkles while it is lit.
            let fate = sparkle_hash(look);
            let birth = SPARKLE_BIRTH * sparkle_bits(fate, 0u, 0xffffu);
            let travel = mix(layer.travel.x, layer.travel.y, sparkle_bits(fate, 16u, 0xffffu));
            let journey = out - birth;
            let left = clamp(1.0 - journey / travel, 0.0, 1.0);
            let life = smoothstep(0.0, 0.015, journey) * left * left * (3.0 - 2.0 * left);
            let twinkle = 0.75 + 0.25 * sin(t * (1.5 + 2.5 * grain) + phase);
            let fade = sparkle_bump(out / lane);
            let amount = (0.35 + 0.65 * strength * strength) * life * twinkle * fade * size;

            // The core is never drawn smaller than a sample or two, and never
            // brighter for being drawn small: spread wider, it is dimmer by
            // exactly the area it gained. Softened, it is spread most of the way
            // to its glow and is little more than the glow.
            let radius = layer.core * size;
            let spread = min(sqrt(radius * radius + 2.25 * sample * sample
                + 0.25 * reach * reach * soften * soften), reach);
            let kept = radius / spread;
            let fall = 1.0 - apart / reach;
            light += amount * vec2<f32>(
                sparkle_bump(apart / spread) * kept * kept,
                fall * fall * fall,
            );
        }
    }
    return light;
}

// One depth of sparkles at a point `offset` below where the ribbon rests: its
// own side of the ribbon, and the other side's too where the point is close
// enough to the ribbon for something just born over there to reach it.
fn sparkle_layer(
    layer: SparkleLayer,
    offset: f32,
    swing: f32,
    along: f32,
    slope: f32,
    lane: f32,
    t: f32,
    sample: f32,
    soften: f32,
) -> vec2<f32> {
    let side = select(-1.0, 1.0, offset >= swing);
    let reach_across = layer.reach * sqrt(1.0 + slope * slope);
    // Nothing of this depth reaches past the lane — and on the side the ribbon
    // has swung away from, past the part of the swing a sparkle out there lags
    // behind. On the side it has swung towards, its hold keeps them inside.
    let lag = select(0.0, (1.0 - layer.hold.x) * abs(swing), side * swing < 0.0);
    if (abs(offset - swing) >= lane + lag + reach_across) {
        return vec2<f32>(0.0);
    }
    var light = sparkle_half(layer, side, offset, swing, along, slope, lane, t, sample, soften);
    if (abs(offset - swing) < reach_across) {
        light += sparkle_half(layer, -side, offset, swing, along, slope, lane, t, sample, soften);
    }
    return light;
}

// The sparkles: glitter the current sheds, the way the original bar's wave
// carried it.
//
// They come out of the ribbon itself. Each is dark until it is inside the body
// of the band, lights there, and moves out of it — up above the ribbon and,
// more slowly, down below it — drifting along the current as it goes and
// fading on the way, so the band is full of them and they thin out around it
// into nothing. Anchored, because every one of them is born in the ribbon and
// carried by it as it swings; loose, because none of them stays where it was
// born. The lane they are shed from is `spine`, which is whichever ribbon the
// wallpaper is drawn with.
//
// Two depths: a fine glitter of many small quick points, and a few larger ones
// with a glow round them. The light is the accent's own soft rung, the core
// washed towards white the way a small bright light overexposes, and nothing
// in it as bright as a label's own light.
fn sparkles(
    into: vec3<f32>,
    uv: vec2<f32>,
    aspect: f32,
    t: f32,
    soften: f32,
    footprint: vec2<f32>,
    spine: Spine,
) -> vec3<f32> {
    let offset = uv.y - SPINE_REST;
    let swing = spine.height - SPINE_REST;
    let slope = clamp(spine.slope, -SPARKLE_STEEPEST, SPARKLE_STEEPEST);
    // Narrower where the band gathers, as the ribbons are.
    let lane = SPARKLE_LANE * (0.45 + 0.55 * spine.gather);
    // Past the edge of the lane — and, on the side the ribbon has swung away
    // from, of the part of the swing a sparkle out there lags behind — and of
    // the reach of any sparkle standing there, there is nothing to find.
    let behind = (offset - swing) * swing < 0.0;
    let lag = select(0.0, (1.0 - min(SPARKLE_DUST.hold.x, SPARKLE_GLINTS.hold.x)) * abs(swing),
                     behind);
    if (abs(offset - swing) >= lane + lag + SPARKLE_GLINTS.reach * sqrt(1.0 + slope * slope)) {
        return into;
    }
    let along = uv.x * aspect;
    let sample = max(footprint.x * aspect, footprint.y);
    let dust = sparkle_layer(SPARKLE_DUST, offset, swing, along, slope, lane, t, sample, soften);
    let glints = sparkle_layer(SPARKLE_GLINTS, offset, swing, along, slope, lane, t, sample, soften);
    let core = dust.x * SPARKLE_DUST.brightness.x + glints.x * SPARKLE_GLINTS.brightness.x;
    let glow = dust.y * SPARKLE_DUST.brightness.y + glints.y * SPARKLE_GLINTS.brightness.y;
    let hot = mix(globals.accent[1].rgb, vec3<f32>(1.0), 0.45);
    let haze = mix(globals.accent[0].rgb, globals.accent[1].rgb, 0.5);
    return into + (hot * core + haze * glow) * mix(1.0, 0.5, soften);
}

// The wallpaper, as a function of where you look rather than as a picture.
//
// Being analytic is what lets anything re-create it: the sliver outside a
// window card's rounded corner, and the glass panes, which redraw it bent at
// their own edges. `soften` stands in for blur — the same scene drawn wide
// and dim rather than pixels filtered, which lands the same impression for
// free. Returns linear light, the space the palette arrives in.
fn wallpaper(
    uv: vec2<f32>,
    aspect: f32,
    t: f32,
    soften: f32,
    lod: f32,
    footprint: vec2<f32>,
) -> vec3<f32> {
    // A wallpaper of the user's own replaces the scene rather than being drawn
    // over it: none of the lights, currents, veils or ribbons below is
    // evaluated, and this is where that stops. It is inside this function
    // rather than at the two places that call it because *everything* asks the
    // wallpaper what is at a point — the pane of glass bending it, the guide
    // softening it, the overview drawing all of it inside a card — and a branch
    // taken anywhere else would leave one of them showing a picture the rest of
    // the screen is not.
    //
    // Two, and never two by accident: the flag is only written while there is
    // really a picture on the GPU to read. See `Gpu::wallpaper_flag`.
    if (globals.style.x > 1.5) {
        return paper_wallpaper(uv, aspect, lod, soften);
    }

    // The mood drifts slowly between the theme's two gradients — indigo to
    // violet and back over a couple of minutes, like the original bar's
    // changing months.
    let mood = 0.5 + 0.5 * sin(t * 0.03);
    let top = mix(globals.sky[0].rgb, globals.sky[2].rgb, mood);
    let bottom = mix(globals.sky[1].rgb, globals.sky[3].rgb, mood);
    // Even the base gradient moves: two very broad currents bend its horizon
    // in opposite directions. There is no seam to cross and no object whose
    // direction could read as gravity.
    let gradient_y = uv.y
        + sin(uv.x * 2.7 + t * 0.075) * 0.045
        + sin(uv.x * 5.3 - t * 0.052) * 0.018;
    // Leave enough night between the moving lights for the selected item to
    // remain the brightest use of the accent on screen.
    var color = mix(top, bottom, smoothstep(0.0, 1.0, gradient_y)) * 0.42;

    // A soft glow behind the cross point keeps the bar area readable.
    let glow_center = vec2<f32>(0.24, 0.34);
    let glow = 1.0 - smoothstep(0.0, 0.8, distance(uv * vec2<f32>(aspect, 1.0),
                                                   glow_center * vec2<f32>(aspect, 1.0)));
    color += globals.glow.rgb * glow * 0.25;

    // A mesh of three display-sized light fields. Their opposing, irrational
    // clocks keep the composition changing without settling into a loop that
    // the eye can follow. A low-frequency warp makes their edges flow instead
    // of exposing the ellipses used to calculate them.
    let p = vec2<f32>((uv.x - 0.5) * aspect, uv.y - 0.5);
    let warp = vec2<f32>(
        sin(p.y * 3.8 + t * 0.11) + sin((p.x + p.y) * 2.1 - t * 0.071),
        sin(p.x * 2.6 - t * 0.093) + sin((p.x - p.y) * 2.4 + t * 0.063),
    ) * 0.035;
    let flowed = p + warp;
    let spread = mix(1.0, 1.45, soften);

    let deep = ambient_field(
        flowed,
        vec2<f32>(
            -aspect * 0.34 + sin(t * 0.083) * aspect * 0.28,
            -0.23 + cos(t * 0.067) * 0.18,
        ),
        vec2<f32>(aspect * 0.36, 0.34) * spread,
    );
    let main = ambient_field(
        flowed,
        vec2<f32>(
            aspect * 0.31 + cos(t * 0.061) * aspect * 0.30,
            0.20 + sin(t * 0.089) * 0.20,
        ),
        vec2<f32>(aspect * 0.34, 0.38) * spread,
    );
    let soft = ambient_field(
        flowed,
        vec2<f32>(
            sin(t * 0.047 + 2.0) * aspect * 0.42,
            sin(t * 0.073 + 1.1) * 0.30,
        ),
        vec2<f32>(aspect * 0.40, 0.29) * spread,
    );

    let field_strength = mix(1.0, 0.48, soften);
    color += globals.accent[2].rgb * deep * 0.16 * field_strength;
    color += globals.accent[0].rgb * main * 0.085 * field_strength;
    color += globals.accent[1].rgb * soft * 0.035 * field_strength;

    // One broad diagonal current puts visible motion between those pools. It
    // is deliberately a continuous rise and fall, never a row of highlights.
    let current = sin(flowed.x * 2.15 + flowed.y * 1.25 + t * 0.13)
        + sin(flowed.x * 0.78 - flowed.y * 2.35 - t * 0.087);
    let current_light = smoothstep(0.32, 1.62, current);
    color += globals.accent[0].rgb * current_light * 0.035
        * mix(1.0, 0.40, soften);

    // The current: the moving thing in the middle of the picture, in
    // whichever material this shell is set to draw it in, and the sparkles it
    // carries — which are the same in either, being light and not material,
    // and which are a setting of their own: `style.w` is one where somebody
    // has turned them on, and nought — which is what every writer of this
    // block that has never heard of them sends — where they are off.
    var spine = spine_at(uv.x, t, aspect);
    if (globals.style.x > 0.5) {
        color = silk(color, uv, aspect, t, soften);
        spine = silk_spine(uv.x, t, aspect);
    } else {
        color = water(color, uv, aspect, t, soften, footprint, spine);
    }
    if (globals.style.w > 0.5) {
        color = sparkles(color, uv, aspect, t, soften, footprint, spine);
    }

    // Two aurora veils sweep through different thirds of the display. Their
    // wide skirts carry most of the light; the crests are only a little
    // brighter, so these read as moving atmosphere rather than drawn lines.
    // Each wave travels against one of its own harmonics, which keeps it
    // billowing in place instead of sliding bodily in any one direction.
    let upper_center = 0.28
        + sin(uv.x * 2.6 + t * 0.16) * 0.10
        + sin(uv.x * 5.4 - t * 0.11) * 0.040;
    let upper_d = uv.y - upper_center;
    let upper_veil = exp(-upper_d * upper_d * mix(32.0, 13.0, soften));
    let upper_crest = exp(-upper_d * upper_d * mix(230.0, 55.0, soften));
    let upper_sheen = 0.72 + 0.28 * sin(uv.x * 4.2 - t * 0.22);

    let lower_center = 0.72
        + sin(uv.x * 2.1 - t * 0.13 + 2.4) * 0.12
        + sin(uv.x * 4.7 + t * 0.083) * 0.035;
    let lower_d = uv.y - lower_center;
    let lower_veil = exp(-lower_d * lower_d * mix(26.0, 11.0, soften));
    let lower_crest = exp(-lower_d * lower_d * mix(180.0, 45.0, soften));
    let lower_sheen = 0.74 + 0.26 * sin(uv.x * 3.7 + t * 0.18 + 1.7);

    let veil_strength = mix(1.0, 0.38, soften);
    color += globals.accent[0].rgb
        * (upper_veil * 0.052 + upper_crest * 0.025)
        * upper_sheen * veil_strength;
    color += (globals.accent[2].rgb * lower_veil * 0.16
        + globals.accent[0].rgb * lower_crest * 0.028)
        * lower_sheen * veil_strength;

    return over_the_wallpaper(color, uv, aspect, lod, soften);
}

// Everything that happens to a wallpaper once it has been drawn, whichever of
// the three it is.
//
// Its own function because there are two wallpapers now — the shell's scene and
// the user's own picture — and these three steps are true of both. A picture
// that skipped them would be the one thing on the display not behaving like
// everything else on it: no game's key art over it, no vignette at the edges,
// and full brightness behind a guide that has dimmed everything else.
fn over_the_wallpaper(
    into: vec3<f32>,
    uv: vec2<f32>,
    aspect: f32,
    lod: f32,
    soften: f32,
) -> vec3<f32> {
    var color = into;

    // The game under the cursor, over everything the shell paints for itself.
    //
    // Over rather than through: the lights, the currents and the silk are the
    // shell being interesting when there is nothing else to look at, and a
    // purple ribbon crawling across somebody's key art is not the shell being
    // interesting. What survives is what comes after this — the vignette and
    // the softening — because those are about the *display*, not about the
    // wallpaper, and a picture that skipped them would be the one thing on
    // screen not behaving like everything else.
    let picture = scenery(uv, aspect, lod);
    let shade = mix(1.0 - SCENERY_SHADE, 1.0, smoothstep(0.0, SCENERY_SHADE_TO, uv.x));
    color = mix(color, picture.rgb * SCENERY_LIGHT * shade, picture.a);

    // Vignette, so the edges do not compete with the content.
    let edge = distance(uv, vec2<f32>(0.5, 0.5));
    color *= 1.0 - smoothstep(0.55, 1.05, edge) * 0.55;

    // A blurred backdrop also steps back in brightness, so the cards drawn
    // over it read as the lit layer — but not so far that the glass laid over
    // it has nothing left to refract.
    return color * mix(1.0, 0.55, soften);
}

// The wallpaper where it is one of the user's own files.
//
// The picture, and then exactly what happens to the shell's own scene: a game's
// key art still lies over it, the edges still fall away, and the guide still
// dims it. What does *not* happen to it is any of the scene — see `wallpaper`,
// which is where this is branched to.
fn paper_wallpaper(uv: vec2<f32>, aspect: f32, lod: f32, soften: f32) -> vec3<f32> {
    return over_the_wallpaper(paper(uv, aspect, lod), uv, aspect, lod, soften);
}

@fragment
fn fs_background(in: BackgroundOut) -> @location(0) vec4<f32> {
    // This pass draws the background in one of three roles, and a pixel is
    // only ever in one of them: the whole surface; the start screen squeezed
    // into its card (a miniature of it, not a crop); or the sliver outside a
    // live window card's rounded corner, repainted with what lies behind it
    // so the compositor's square-cornered window appears rounded.
    let px = in.uv * globals.resolution;
    let radius = globals.params.x;
    var uv = in.uv;
    var coverage = 1.0;
    var soften = globals.blur;
    // How much of the wallpaper one pixel of this pass stands for. The whole
    // surface draws it a display wide; the miniature draws all of it inside a
    // card, where one pixel is worth several of the display's, and the band of
    // water has to know that or its edges arrive as a staircase.
    var footprint = 1.0 / globals.resolution;

    // The miniature fades up with the rest of its card, and the wallpaper
    // inside it is drawn here rather than in the scene, so the fade has to
    // reach this pass too. A corner cover is never faded: it is a repair to
    // what is already on screen, not something arriving.
    var fade = 1.0;

    if (globals.window_rect.z > 0.0 && globals.window_rect.w > 0.0) {
        let lo = globals.window_rect.xy;
        let size = globals.window_rect.zw;
        uv = (px - lo) / size;
        footprint = 1.0 / size;
        coverage = rounded_coverage(px, globals.window_rect, radius);
        fade = globals.params.w;
    }

    let covers = u32(globals.params.z);
    for (var i = 0u; i < covers; i = i + 1u) {
        let card = globals.covers[i];
        if (card.z <= 0.0 || card.w <= 0.0) {
            continue;
        }
        // Reaching past the card, not just into its corners: a client whose
        // surface runs beyond the window geometry it declared — a drop shadow,
        // a border it paints but does not count — hangs a square-edged strip
        // off the card, outside the corners this pass would round. The card is
        // the shape the shell framed, so everything up to a margin outside it
        // goes back to being wallpaper.
        // Sized from the card, so it stays inside the space between cards
        // however big they are: the column's gap is 0.09 of the display and a
        // card is 0.54 of it, which puts 8% of a card just under half the gap.
        let trim = max(min(card.z, card.w) * 0.08, radius);
        let inside_bounds = all(px >= card.xy - trim)
            && all(px <= card.xy + card.zw + trim);
        if (inside_bounds) {
            // Outside the rounded shape: the corner the window would otherwise
            // show square, and anything of it hanging past the card at all.
            let corner = 1.0 - rounded_coverage(px, card, radius);
            if (corner > 0.0) {
                coverage = corner;
                uv = in.uv;
                footprint = 1.0 / globals.resolution;
                soften = globals.params.y;
                fade = 1.0;
            }
        }
    }

    let aspect = globals.resolution.x / max(globals.resolution.y, 1.0);
    // The analytic wallpaper answers `soften` by drawing itself wide and dim;
    // a photograph can only answer it by being sampled off a smaller copy of
    // itself, so the same ramp has to reach it as a rung of its blur chain.
    let color = wallpaper(uv, aspect, globals.time, soften, soften * SCENERY_LEVELS, footprint);

    // Premultiplied, so the surface blends correctly where the background
    // does not reach.
    let alpha = coverage * fade;
    return vec4<f32>(color * alpha, alpha);
}

// ---------------------------------------------------------------------------
// Instanced quads: icons and panels.
// ---------------------------------------------------------------------------

struct QuadIn {
    // x, y, width, height in pixels.
    @location(0) rect: vec4<f32>,
    // u0, v0, u1, v1.
    @location(1) uv: vec4<f32>,
    @location(2) color: vec4<f32>,
    // Corner radius, outline thickness and notch half-width in pixels, then
    // frost.
    @location(3) shape: vec4<f32>,
    // Slab depth in pixels, the blur of what is behind, gloss and opacity.
    @location(4) material: vec4<f32>,
    // The norm the corners are cut to: 2 circular, 4 a squircle.
    @location(5) corner_power: f32,
    // A shallow optical bow across a large pane's reflective face.
    @location(6) face_curve: f32,
    // How much of the colour is taken out of what is sampled: 0 the picture as
    // it was made, 1 grey.
    @location(7) drain: f32,
    // Which material one of the shell's own marks is drawn in, or a negative
    // to draw it in whatever the theme says. See `glyph_material`.
    @location(8) mark: f32,
    // The rectangle this pane is cut to, as its two corners in pixels. A pane
    // nothing is cutting carries a box larger than any display.
    @location(9) cut: vec4<f32>,
};

struct QuadOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    // Position within the quad and its half size, both in pixels, for the
    // rounded-corner distance field.
    @location(2) local: vec2<f32>,
    @location(3) half_size: vec2<f32>,
    @location(4) shape: vec4<f32>,
    @location(5) material: vec4<f32>,
    @location(6) corner_power: f32,
    @location(7) face_curve: f32,
    @location(8) drain: f32,
    @location(9) mark: f32,
    @location(10) cut: vec4<f32>,
};

@vertex
fn vs_quad(@builtin(vertex_index) index: u32, quad: QuadIn) -> QuadOut {
    // Two triangles, as a corner lookup.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(1.0, 1.0),
        vec2<f32>(0.0, 1.0),
    );
    let corner = corners[index];

    let position = quad.rect.xy + corner * quad.rect.zw;

    var out: QuadOut;
    out.clip = to_clip(position);
    out.uv = mix(quad.uv.xy, quad.uv.zw, corner);
    out.color = quad.color;
    out.local = corner * quad.rect.zw;
    out.half_size = quad.rect.zw * 0.5;
    out.shape = quad.shape;
    out.material = quad.material;
    out.corner_power = quad.corner_power;
    out.face_curve = quad.face_curve;
    out.drain = quad.drain;
    out.mark = quad.mark;
    out.cut = quad.cut;
    return out;
}

@group(1) @binding(0) var atlas_texture: texture_2d<f32>;
@group(1) @binding(1) var atlas_sampler: sampler;

// The frame as it stood before this run of quads began, with a chain of
// ever-blurrier copies below it. This is what glass looks *through*.
@group(2) @binding(0) var backdrop_texture: texture_2d<f32>;
@group(2) @binding(1) var backdrop_sampler: sampler;

// Which way the surface faces at `point`: the gradient of the distance field,
// which for a rounded rectangle points straight out of the nearest edge and
// turns smoothly through the corners. It is what lets a pane be lit from one
// direction instead of outlined evenly all the way round.
fn edge_normal(point: vec2<f32>, half: vec2<f32>, radius: f32, power: f32) -> vec2<f32> {
    let e = vec2<f32>(1.0, 0.0);
    let gradient = vec2<f32>(
        rounded_box(point + e.xy, half, radius, power)
            - rounded_box(point - e.xy, half, radius, power),
        rounded_box(point + e.yx, half, radius, power)
            - rounded_box(point - e.yx, half, radius, power),
    );
    let len = length(gradient);
    if (len < 0.0001) {
        return vec2<f32>(0.0, -1.0);
    }
    return gradient / len;
}

// --- the glass ------------------------------------------------------------
//
// A pane is treated as a real slab: flat on top, its rim rounded over, its
// underside level, floating a little way above whatever it is laid on. Every
// number below describes that object rather than the picture of it, and the
// picture falls out of tracing one ray per wavelength through it. That is the
// difference between glass and a rectangle with a bright edge painted on.

// Index of refraction. Around that of a dense optical crown glass; higher
// bends more, and past about 1.6 the rim starts to look like a soap bubble.
const GLASS_IOR: f32 = 1.47;

// How much further apart the red and the blue ends of the spectrum sit in it.
// Real glass disperses by well under a hundredth of this, but a pane is a few
// pixels of edge rather than a few centimetres of prism, and without the
// exaggeration the colour never becomes visible at all.
const GLASS_DISPERSION: f32 = 0.055;

// How far a pane floats above what it refracts, as a multiple of its own
// depth. The leg through the glass is bounded by the slab's thickness and on
// its own displaces almost nothing; it is the leg *after* the ray leaves the
// underside, still slanted, that does the visible work. This is the number
// that decides how much of the background gets dragged into the rim.
const GLASS_FLOAT: f32 = 1.0;

// The deepest rung of the blur chain a pane may ask for. One less than the
// number of rungs the renderer builds — `BACKDROP_MIPS` — and the two have to
// agree, or frost samples a rung nothing ever rendered.
const BACKDROP_LEVELS: f32 = 4.0;

// What a fully frosted surface glows with on its own.
//
// Frost is scattering, and a scattering layer does not only blur what is
// behind it: it catches the light falling on it and hands some of it straight
// back. That pale cast is why a frosted pane laid over something black still
// reads as a pane rather than as a hole cut in the screen. Its own constant
// rather than a share of the environment above, which is a *lamp* — bright on
// purpose, because it is about to be multiplied by a four percent reflectance,
// and nothing that gets added to a surface unmultiplied should be that size.
const FROST_SCATTER: vec3<f32> = vec3<f32>(0.030, 0.029, 0.046);

// How steep the bevel is `inset` of the way in: 0 at the outer lip, 1 where
// the flat face begins.
//
// A quarter-round profile. It stands vertical at the very lip — which is where
// glass bends light hardest — and lies flat across the face, which is why the
// middle of a pane shows what is behind it undistorted and only its edges do
// anything. `rise` is how much of the full depth has been reached there.
fn bevel_rise(inset: f32) -> f32 {
    return sqrt(max(1.0 - (1.0 - inset) * (1.0 - inset), 0.0));
}

// Held short of vertical at the very lip. A truly vertical face is edge-on to
// a viewer looking straight down at it — no projected area, no reflection, and
// a normal the refraction and the lighting both degenerate on. What the eye
// wants there is the last pixel of a steep curve, not the singularity.
fn bevel_slope(inset: f32) -> f32 {
    return (1.0 - inset) / max(bevel_rise(inset), 0.16);
}

// How far the bevel shifts the ray arriving `inset` of the way into it, in
// pixels, measured outward — so a negative number, because glass pulls what is
// behind it towards the middle of the pane.
//
// Straight down out of the viewer's eye, into the sloped bevel, across the
// glass, out of the level underside, and on through the gap below. `eta` is
// the air-to-glass ratio for one wavelength: asking three slightly different
// ones is what splits white light into colour at a steep edge.
//
// A scalar, and only a scalar, because the answer always points along the
// surface's own outward normal — every term below is built from it.
fn bevel_shift(inset: f32, slab: f32, eta: f32) -> f32 {
    let rise = bevel_rise(inset);
    let normal = normalize(vec3<f32>(bevel_slope(inset), 0.0, 1.0));
    let inside = refract(vec3<f32>(0.0, 0.0, -1.0), normal, eta);

    // The leg through the slab itself, which is `slab * rise` deep here.
    var shift = inside.x * (slab * rise / max(-inside.z, 0.05));

    // And out. The underside is level, so the ray straightens back towards the
    // angle it would have had through a plain sheet — but not all the way to
    // vertical, because it came in through a slope. Snell again, the other way
    // round. This leg is the long one: it is the gap under the pane, not the
    // glass in it, that decides how much gets dragged into the rim.
    let sin_in = abs(inside.x);
    if (sin_in > 1e-5) {
        // Held just short of grazing. At the very lip the exit ray lies down
        // almost flat and the shift would run away to the far side of the
        // display; Fresnel has turned that sliver into a mirror by then, so
        // nothing is lost by not following it there.
        let sin_out = min(sin_in / eta, 0.90);
        let cos_out = sqrt(max(1.0 - sin_out * sin_out, 1e-4));
        shift += sign(inside.x) * (sin_out / cos_out) * slab * GLASS_FLOAT;
    }
    return shift;
}

// What is behind the pane at `px`, softened by `lod` rungs of the blur chain.
//
// Two things are behind it, and this is where they are put back together.
// Everything the shell has already drawn into this frame is read from the
// snapshot, premultiplied — icons, cards, the panel a button is resting on.
// Wherever it drew nothing, what shows through is the wallpaper on the second
// Wayland surface below this one, which the shell cannot read back but can
// *evaluate*, because it is the same function that painted it.
fn behind_at(px: vec2<f32>, lod: f32, soften: f32) -> vec3<f32> {
    let uv = clamp(px / globals.resolution, vec2<f32>(0.0), vec2<f32>(1.0));
    let drawn = textureSampleLevel(backdrop_texture, backdrop_sampler, uv, lod);
    let aspect = globals.resolution.x / max(globals.resolution.y, 1.0);
    // Frost blurs what a pane transmits, and that has to include a picture:
    // the analytic wallpaper is smooth enough that scattering it changes
    // little, while a photograph seen sharp through deep frost is a pane with
    // a hole in it. The deeper of the two — the frost's own rung and the
    // softness the wallpaper is being drawn at — is what it is asked for.
    let below = wallpaper(
        uv,
        aspect,
        globals.time,
        soften,
        max(lod, soften * SCENERY_LEVELS),
        1.0 / globals.resolution,
    );
    return drawn.rgb + below * (1.0 - drawn.a);
}

// What a pane sees reflected in itself.
//
// No environment map to reflect, so: a bright quarter above the shell and a
// dark one below it, and the key light as a tight highlight in it. Crude, but
// it is looked up through the true mirror direction, so it slides round a
// corner and travels along an edge the way a reflection does, rather than
// sitting wherever a gradient was drawn.
//
// Bright numbers, because they are about to be multiplied by a dielectric's
// reflectance — four percent, straight on. A lamp is thousands of times
// brighter than the surface it is lit by, and four percent of something dim is
// the nothing the shell used to have at its rims.
fn environment(mirrored: vec3<f32>, key_strength: f32) -> vec3<f32> {
    // Only what the mirror direction actually points *up* at is bright.
    // Everything else in this room is the shell's own dark wallpaper, and that
    // includes the direction a grazing rim reflects in — which is along the
    // surface and into the scene, not up. Getting that wrong is what puts a
    // flat grey outline round every pane.
    let sky = clamp(-mirrored.y, 0.0, 1.0);
    let ambient = mix(
        vec3<f32>(0.03, 0.03, 0.05),
        vec3<f32>(1.20, 1.16, 1.45),
        sky * sky,
    );
    let key = pow(max(dot(mirrored, normalize(KEY_LIGHT)), 0.0), 36.0);
    return ambient + vec3<f32>(1.0, 0.98, 0.93) * key * 26.0 * key_strength;
}

// --- a glyph shaded out of its own shape -----------------------------------
//
// Every one of the shell's own glyphs ships as a *measurement* rather than a
// picture: its cell holds how far each pixel is from the nearest edge of the
// mark, negative inside it. See `icons::builtin_distance_field`, which makes
// the field, and the head of category-games.svg, which is the drawing the
// language was settled on.
//
// What that buys is one piece of code arriving at an edge. A hole's edge is an
// edge like any other, so the ring standing round every opening in a glyph —
// the displaced volume, the most expensive thing to draw by hand and the thing
// that was redrawn most often — falls out of the same three lines as the outer
// rim, and nothing below knows the mark has holes in it at all.
//
// One thing a pane does that a glyph deliberately does not: refract. A pane
// reads the frame beneath it, which costs a snapshot and a blur chain, and
// `Quad::reads_backdrop` will not hand one to a glyph — a settings panel draws
// a dozen of them over surfaces drawn moments earlier in the same run, and
// every one would want a snapshot of its own. Past the batch budget they would
// share an older one instead and refract the *wallpaper* into a rim that is
// lying on a pane, which is worse than not refracting: a mark with the wrong
// room in its edge. So a glyph is lit and reflective, and what it transmits is
// its own colour.

// Half the range a glyph's distance field spans, as a fraction of its atlas
// cell. `icons::SDF_RANGE` encodes it and this undoes it; the two are one
// number and have to move together.
const GLYPH_SDF_RANGE: f32 = 0.125;

// Edge of one atlas cell in texels, which is `gpu::CELL`. The shader needs it
// to know what a slope of one looks like in the field it is reading.
const GLYPH_CELL: f32 = 128.0;

// Where the light is, for every glyph in the shell at once — up and to the
// left, tilted towards the viewer. Not `KEY_LIGHT`, which is aimed across a
// whole display and would walk over a mark as its column scrolled: a glyph is
// an object held up to be looked at, and the light on it stays put.
//
// Written out rather than normalised at run time so that the shadow and the
// highlight below cannot drift apart.
const GLYPH_LAMP: vec3<f32> = vec3<f32>(-0.4915, -0.7078, 0.5069);

// How dark the mark's own shadow is where it meets the flat space.
const GLYPH_SHADOW: f32 = 0.30;

// The plain material, for the Simple theme: how much of the accent is mixed
// into a mark, and how solid the mark is.
//
// White with the accent breathed over it rather than the accent itself — the
// shell's marks are read at a glance and a coloured mark is a slower one — and
// a little short of solid, because every one of them stands on glass and a mark
// that is *more* opaque than the pane it is on reads as a sticker on it.
const GLYPH_FLAT_TINT: f32 = 0.22;
const GLYPH_FLAT_ALPHA: f32 = 0.88;
// How much of the colour the caller asked for survives. Nearly none: the marks
// are white in this theme. Not *quite* none, so that a caller which deliberately
// draws one dark — on a light surface — still has something visible there.
const GLYPH_FLAT_STAIN: f32 = 0.15;

// How far this pixel is from the nearest edge of the glyph, in cell fractions.
// Negative inside it.
//
// `textureSampleLevel` rather than `textureSample`: this is read inside a
// branch, and a sample that works out its own level of detail may not be.
fn glyph_distance(uv: vec2<f32>) -> f32 {
    let stored = textureSampleLevel(atlas_texture, atlas_sampler, uv, 0.0).a;
    return (stored - 0.5) * 2.0 * GLYPH_SDF_RANGE;
}

// The same, held inside one cell of the atlas.
//
// Every read below is offset from the pixel being drawn — the gradient looks a
// texel either side, and the shadow looks a good deal further than that — so a
// pixel near the edge of a mark reads past the edge of its own cell and finds
// the glyph stored next to it. What that looks like is a hairline of somebody
// else's shadow ruled along the top and left of every glyph in the shell, which
// is how it was found. Half a texel in from the boundary, because the sampler
// is bilinear and would otherwise still blend in the neighbour.
fn glyph_at(uv: vec2<f32>, cell: vec4<f32>) -> f32 {
    return glyph_distance(clamp(uv, cell.xy, cell.zw));
}

fn glyph_material(in: QuadOut) -> vec4<f32> {
    let size = in.half_size * 2.0;
    let texel = 1.0 / vec2<f32>(textureDimensions(atlas_texture));
    // One pixel of the drawn glyph, in the atlas coordinates it is sampled by.
    // The cell is drawn across the whole quad, so this is the conversion that
    // makes a bevel the same width at 84 pixels and at 148.
    let per_pixel = GLYPH_CELL * texel / max(size, vec2<f32>(1.0));

    // Which cell of the atlas this glyph is in, worked back out of what the
    // vertex stage handed over: the pixel's place inside the quad says how far
    // into the cell its sample is, and the whole cell is drawn across the whole
    // quad. Half a texel in on each side, for the sampler.
    let half_texel = texel * 0.5;
    let corner = in.uv - in.local * per_pixel;
    let cell = vec4<f32>(corner + half_texel, corner + size * per_pixel - half_texel);

    // How far into the mark this pixel is, in pixels of the drawn glyph.
    let d = glyph_at(in.uv, cell) * size.x;
    let coverage = 1.0 - smoothstep(-0.75, 0.75, d);

    // Which material this mark is drawn in. The theme's answer for the marks —
    // `globals.style.y`, not the wallpaper's `x`: the drawing and nothing else
    // — unless the quad brought one of its own, which four rows in the whole
    // shell do.
    //
    // Those four are the Theme page's own values, and each of them wears the
    // same drawing as the row above or below it: what says which is which is
    // that each is drawn in the material it applies. See `Quad::mark`, which
    // is where the argument for that is written down.
    let plain = select(globals.style.y, in.mark, in.mark >= 0.0);

    // The Simple theme, as chosen for the *marks*.
    //
    // The *shape* is identical — this is the same distance field, read at the
    // same edge, so a mark is the same mark and antialiases the same way. What
    // is dropped is everything that made it a body: the gradient of the field,
    // the bevel, the fall down its own height, the reflection, the specular,
    // the dispersion and its own shadow. Nine texture reads become one and the
    // arithmetic becomes a fill, which is the whole point of the theme.
    if (plain > 0.5) {
        let stain = mix(vec3<f32>(1.0), in.color.rgb, GLYPH_FLAT_STAIN);
        let flat = mix(stain, globals.accent[1].rgb, GLYPH_FLAT_TINT);
        return vec4<f32>(flat, coverage * GLYPH_FLAT_ALPHA * in.material.w);
    }

    // Which way the surface faces: the gradient of the field, which for a
    // distance field points straight out of the nearest edge wherever it is
    // read. Central differences, so a wall reads the same from either side.
    //
    // Read a texel and a half out rather than one, and the sampler interpolates
    // the half for nothing. The field is eight bits over a quarter of the cell,
    // so one texel of travel is about eight levels of it and a difference taken
    // that close is a tenth quantisation noise — which arrives as mottling
    // across the face of a mark, since a bevel this deep leaves most of a small
    // one *in* the bevel. The wider arm doubles the signal against the same
    // noise, and it also softens the medial ridge below into something that
    // falls off over a few pixels instead of switching.
    let arm = texel * 1.5;
    let east = glyph_at(in.uv + vec2<f32>(arm.x, 0.0), cell);
    let west = glyph_at(in.uv - vec2<f32>(arm.x, 0.0), cell);
    let south = glyph_at(in.uv + vec2<f32>(0.0, arm.y), cell);
    let north = glyph_at(in.uv - vec2<f32>(0.0, arm.y), cell);
    let gradient = vec2<f32>(east - west, south - north);
    let outward = normalize(gradient + vec2<f32>(1e-6, 0.0));

    // How much of a slope the field actually has here. A distance field rises
    // at exactly one per pixel everywhere *except* along the skeleton of the
    // shape — the ridge equidistant from two edges — where two slopes meet
    // head on and cancel. Every part of a mark narrower than twice the bevel
    // has that ridge running down the middle of it, and a surface built from
    // the field alone creases along it: thin parts come out faceted, which is
    // the one artefact that says "computed" rather than "wet".
    //
    // The cancellation is also how to find it. Where the slope falls away the
    // surface is flattened towards level, which is what the middle of a narrow
    // run of water does anyway.
    let slope = clamp(length(gradient) / (2.0 * 1.5 / GLYPH_CELL), 0.0, 1.0);
    let ridge = smoothstep(0.20, 0.80, slope);

    let slab = max(in.material.x, 0.5);
    // Where in the rounding-over this pixel is: 0 at the outer lip, 1 where
    // the flat face begins. A part of the mark thinner than twice the slab
    // never reaches 1 and is therefore all bevel — which is correct, and is
    // the thing a hand-drawn rim has to be told one shape at a time.
    let inset = clamp(-d / slab, 0.0, 1.0);
    let surface = normalize(vec3<f32>(outward * bevel_slope(inset) * ridge, 1.0));

    // The one thing the field cannot know: which way is up. A bead of water
    // stands its wall up at the crown, where it is nearly edge-on and hands
    // over the room behind it, and lays it down at the foot, where the whole
    // lamp arrives at once. So the mark is taken on down its own height rather
    // than evenly across it — the same fall the authored drawings paint, as
    // one line instead of a gradient with five stops.
    let foot = clamp(in.local.y / max(size.y, 1.0), 0.0, 1.0);
    var glass = in.color.rgb * mix(0.50, 1.0, foot * foot);
    // Alpha follows the same fall: the crown is where a bead lets the room
    // through, and the foot is where it has gathered enough of itself to be
    // solid.
    var alpha = mix(0.52, 0.94, foot * foot);

    // Where the wall is turned into the lamp and where it is turned away,
    // which is the whole of the modelling and the one thing the fall above
    // cannot say: a bead's crown is dark because its wall stands up *there*,
    // not because it is high up.
    let facing = clamp(dot(surface, GLYPH_LAMP), 0.0, 1.0);
    glass = glass * mix(0.70, 1.26, facing);

    // A thick edge splits what passes through it into colour. There is nothing
    // behind this mark to split, so the same thing is done to its own light:
    // the two ends of the spectrum are pushed a little way apart along the
    // outward normal, which puts warmth on one side of a rim and cold on the
    // other exactly where the wall is steepest. Small, because it is a cue and
    // not a prism.
    let edge = (1.0 - inset) * (1.0 - inset) * ridge;
    let split = GLASS_DISPERSION * edge * outward.x;
    glass = glass * vec3<f32>(1.0 + split, 1.0, 1.0 - split);

    let gloss = in.material.z;
    if (gloss > 0.0) {
        let fresnel = 0.04 + 0.96 * pow(1.0 - clamp(surface.z, 0.0, 1.0), 5.0);
        let lit = fresnel * gloss;
        // A sky with no horizon in it. `environment` has one — a bright band
        // where its two halves meet — which a pane wants and a mark cannot
        // afford: reflected in a small curved shape it lands as a straight
        // line across the middle of the drawing and reads as a crack. Its
        // colour belongs to the panes too; a glyph is tinted by whatever asked
        // for it, so only the brightness of the reflection is kept.
        let mirrored = reflect(vec3<f32>(0.0, 0.0, -1.0), surface);
        let value = mix(0.42, 1.15, 0.5 + 0.5 * dot(mirrored, -GLYPH_LAMP));
        glass = mix(glass, in.color.rgb * value, lit);
        // And the specular proper: a small hard reflection of the lamp itself,
        // which on a curved wall lies along the curve and is the mark of a wet
        // surface rather than a matte one.
        let half_way = normalize(GLYPH_LAMP + vec3<f32>(0.0, 0.0, 1.0));
        let spec = pow(clamp(dot(surface, half_way), 0.0, 1.0), 42.0);
        glass = glass + in.color.rgb * spec * gloss * 0.9;
        alpha = max(alpha, max(lit, spec));
    }

    // The mark on the flat space it is standing on, which is the last thing
    // the authored drawings paint by hand and the first thing that says the
    // glyph is an object rather than a hole. The occluder is up-light of the
    // shadow, so the field is read once in the lamp's direction: wherever
    // *that* is inside the mark, this pixel is in its shade.
    //
    // Tight, and it has to be: the shadow can only be drawn where the quad
    // reaches, and what it has to fit inside is the margin every glyph leaves
    // round its mark — two of the drawing's thirty-two units, which the
    // built-in glyph test measures. Offset and penumbra together come to under
    // five hundredths of the mark's own size, so nothing ends in a straight
    // cut at the edge of the cell. Which is also the right shadow: a bead is
    // *on* the surface, not floating over it.
    let toward_lamp = normalize(GLYPH_LAMP.xy) * slab * 0.22;
    let occluder = glyph_at(in.uv + toward_lamp * per_pixel, cell) * size.x;
    let blocked = 1.0 - smoothstep(-slab * 0.1, slab * 0.4, occluder);
    let shade = blocked * GLYPH_SHADOW * (1.0 - coverage);

    // The mark composited over its own shadow, in straight alpha because that
    // is what the pipeline blends with.
    let mark = alpha * coverage;
    let total = mark + shade * (1.0 - mark);
    let rgb = max(glass, vec3<f32>(0.0)) * mark / max(total, 1e-4);
    return vec4<f32>(rgb, total * in.material.w);
}

@fragment
fn fs_quad(in: QuadOut) -> @location(0) vec4<f32> {
    // What something in front cut away, or what a display cut off its own
    // edge. The shape is untouched — this pane is still the whole pane, lit
    // and bevelled as one — and only the pixels outside the rectangle are
    // dropped, which is exactly what an edge does to what runs past it.
    if (in.clip.x < in.cut.x || in.clip.y < in.cut.y
        || in.clip.x > in.cut.z || in.clip.y > in.cut.w) {
        discard;
    }

    // A cell that holds the shape of a glyph rather than a picture of one, to
    // be shaded as a bead of water standing on the surface below it.
    //
    // Nothing else in the shell asks for a square-cornered quad with a depth:
    // a pane that wants the material is rounded, and a picture that wants the
    // atlas sampled straight carries no depth. So the pair is how a caller
    // says which kind of cell it is pointing at, without a field of its own —
    // and `Quad::glyph_material` is the same test, written once on the other
    // side so the two cannot drift.
    if (in.shape.x <= 0.0 && in.material.x > 0.0) {
        return glyph_material(in);
    }

    let texel = textureSample(atlas_texture, atlas_sampler, in.uv);
    var color = texel * in.color;

    // The colour taken out of it, for a cover whose game is not on this disk.
    // Rec.709 luminance in the linear light the atlas is sampled in, so what
    // is left is how bright the picture actually is — a mean of three channels
    // would darken every red and lighten every blue, which on a wall of covers
    // reads as the pictures having been damaged rather than drained.
    if (in.drain > 0.0) {
        let lit = dot(color.rgb, vec3<f32>(0.2126, 0.7152, 0.0722));
        color = vec4<f32>(mix(color.rgb, vec3<f32>(lit), in.drain), color.a);
    }

    // A radius of zero is the plain rectangle, deliberately without the edge
    // antialiasing below: hairline rules and icon quads must not be softened
    // by a shape they never asked for.
    let radius = in.shape.x;
    if (radius > 0.0) {
        let point = in.local - in.half_size;
        let power = in.corner_power;
        let d = rounded_box(point, in.half_size, radius, power);
        var coverage = 1.0 - smoothstep(-0.75, 0.75, d);

        // --- glass -------------------------------------------------------
        // Only for filled panes: a ring is already an edge, and giving one a
        // thickness would just make it lopsided.
        let thickness = in.material.x;
        let gloss = in.material.z;
        let frost = in.shape.w;
        if (in.shape.y <= 0.0 && (thickness > 0.0 || gloss > 0.0)) {
            // How deep the slab is, and how wide its rim is rounded over —
            // one number, as on a real edge. A small control takes a
            // proportionally shallower slab, or a button would be all bevel
            // and no face.
            let slab = min(thickness, min(in.half_size.x, in.half_size.y) * 0.44);

            // Where in that rounding this pixel is: 0 at the outer lip, 1
            // where the flat face begins.
            let inset = select(1.0, clamp(-d / slab, 0.0, 1.0), slab > 0.0);
            let rise = bevel_rise(inset);
            let outward = edge_normal(point, in.half_size, radius, power);
            // The surface, in three dimensions at last. Compact panes stay
            // level over the face; a broad sheet may carry a very shallow bow
            // so the room reflected in it changes across more than its rim.
            // `rise` eases that bow out at the lip, where the real bevel is
            // already the surface and must remain continuous.
            let face_position = point / max(in.half_size, vec2<f32>(1.0));
            let face_slope = face_position
                * vec2<f32>(0.28, 0.45)
                * in.face_curve
                * rise;
            let surface = normalize(vec3<f32>(
                outward * bevel_slope(inset) + face_slope,
                1.0));

            var glass = color.rgb;
            var alpha = color.a;

            if (slab > 0.0) {
                // Frost scatters what the pane transmits: the deeper into it,
                // the further down the blur chain. The rim stays comparatively
                // clear, because bending a field already blurred to a flat
                // colour is bending nothing — which is exactly why the lens
                // this replaces was invisible.
                let lod = frost * BACKDROP_LEVELS * (0.3 + 0.7 * inset);
                // The wallpaper is on another surface and is softened there,
                // not here, so it has to be asked for at the softness it is
                // actually being drawn with — or a pane shows a sharp
                // wallpaper against the blurred one it is lying on.
                let soften = clamp(max(in.material.y, frost), 0.0, 1.0);

                let eta = 1.0 / GLASS_IOR;
                let spread = GLASS_DISPERSION * eta;
                let shift = bevel_shift(inset, slab, eta);
                // Three wavelengths. One is what makes a lens look drawn.
                let red = behind_at(
                    in.clip.xy + outward * bevel_shift(inset, slab, eta + spread), lod, soften);
                let green = behind_at(in.clip.xy + outward * shift, lod, soften);
                let blue = behind_at(
                    in.clip.xy + outward * bevel_shift(inset, slab, eta - spread), lod, soften);

                // A lens does not only move light, it concentrates it: where
                // the bevel squeezes a wide band of the background into a
                // narrow one, the same light arrives over fewer pixels and the
                // rim brightens. That thin bright line along the edge of
                // anything thick and clear is a caustic, and this is it —
                // measured off how fast the sample runs away from the pixel
                // rather than drawn on as a highlight.
                let step = 0.02;
                let sample_moves = (bevel_shift(inset + step, slab, eta) - shift) / (step * slab);
                let caustic = clamp(1.0 / max(abs(1.0 - sample_moves), 0.3), 0.45, 2.6);

                // Stained by how much glass the ray actually crossed, so a rim
                // is clearer than a face. That is how a tinted slab behaves,
                // and it is most of how the eye tells one from a flat shape
                // with the same colour in it.
                let stain = color.a * mix(0.4, 1.0, rise);
                glass = mix(vec3<f32>(red.r, green.g, blue.b) * caustic, color.rgb, stain);
                glass += FROST_SCATTER * frost;
                // A pane with any depth is reproducing what is behind it
                // rather than letting it through, so it owns its pixels.
                alpha = 1.0;
            }

            if (gloss > 0.0) {
                // Schlick. A glass surface turns to mirror as the line of
                // sight lies down along it, which around a rounded rim happens
                // by itself, all the way round, in exactly the proportion the
                // corner curves. The band this replaces was a smoothstep
                // pretending to be this.
                let fresnel = 0.04 + 0.96 * pow(1.0 - clamp(surface.z, 0.0, 1.0), 5.0);
                let lit = fresnel * gloss;
                // A curved face spreads the lamp over a broad patch. Keep the
                // tight, full-strength source on the true bevel, but soften it
                // over that large reflection so it reads as sheen rather than
                // a white decal laid across the panel.
                let broad_face = smoothstep(0.68, 1.0, inset)
                    * clamp(in.face_curve, 0.0, 1.0);
                let key_strength = mix(1.0, 0.10, broad_face);
                glass = mix(
                    glass,
                    environment(
                        reflect(vec3<f32>(0.0, 0.0, -1.0), surface),
                        key_strength),
                    lit);
                // A reflection is opaque: it is light coming off the front of
                // the pane, not through it. Without this a rim on a pane you
                // can see through is light added to almost nothing, which is
                // still almost nothing.
                alpha = max(alpha, lit);
            }

            color = vec4<f32>(max(glass, vec3<f32>(0.0)), alpha);
        }

        // A border thickness turns the fill into an outline that follows the
        // corners all the way round, which four straight runs cannot.
        let border = in.shape.y;
        if (border > 0.0) {
            coverage = coverage * smoothstep(-0.75, 0.75, d + border);
        }

        // A notch cuts a vertical slot through the upper half. It exists for
        // one shape — the power glyph, a ring broken at the top with a stroke
        // rising through the break — which cannot be drawn by painting over
        // the ring, because everything behind this surface shows through it.
        let notch = in.shape.z;
        if (notch > 0.0 && in.local.y <= in.half_size.y) {
            let dx = abs(in.local.x - in.half_size.x);
            coverage = coverage * smoothstep(notch - 0.75, notch + 0.75, dx);
        }

        color.a = color.a * coverage;
    }

    // The pane's own opacity, kept apart from the tint so a glass pane can
    // fade out without becoming clear glass on its way.
    color.a = color.a * in.material.w;
    return color;
}
