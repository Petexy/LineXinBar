// Shaders for the XMB shell.
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
};

@group(0) @binding(0) var<uniform> globals: Globals;

// Steam's own pictures of the games under the cursors, one per layer, each
// with a chain of ever-smaller copies for the blur. Bound to both passes that
// draw the wallpaper, because the wallpaper is one function and it samples
// this.
@group(3) @binding(0) var scenery_texture: texture_2d_array<f32>;
@group(3) @binding(1) var scenery_sampler: sampler;

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
    if (leaving > 0.0) {
        color += textureSampleLevel(
            scenery_texture, scenery_sampler, cropped, i32(globals.hero.x), lod).rgb * leaving;
    }
    if (arriving > 0.0) {
        color += textureSampleLevel(
            scenery_texture, scenery_sampler, cropped, i32(globals.hero.y), lod).rgb * arriving;
    }
    return vec4<f32>(color / total, min(total, 1.0));
}

// The wallpaper, as a function of where you look rather than as a picture.
//
// Being analytic is what lets anything re-create it: the sliver outside a
// window card's rounded corner, and the glass panes, which redraw it bent at
// their own edges. `soften` stands in for blur — the same scene drawn wide
// and dim rather than pixels filtered, which lands the same impression for
// free. Returns linear light, the space the palette arrives in.
fn wallpaper(uv: vec2<f32>, aspect: f32, t: f32, soften: f32, lod: f32) -> vec3<f32> {
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

    // The XMB current: three fine glass-silk ribbons moving together through
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

    // The miniature fades up with the rest of its card, and the wallpaper
    // inside it is drawn here rather than in the scene, so the fade has to
    // reach this pass too. A corner cover is never faded: it is a repair to
    // what is already on screen, not something arriving.
    var fade = 1.0;

    if (globals.window_rect.z > 0.0 && globals.window_rect.w > 0.0) {
        let lo = globals.window_rect.xy;
        let size = globals.window_rect.zw;
        uv = (px - lo) / size;
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
                soften = globals.params.y;
                fade = 1.0;
            }
        }
    }

    let aspect = globals.resolution.x / max(globals.resolution.y, 1.0);
    // The analytic wallpaper answers `soften` by drawing itself wide and dim;
    // a photograph can only answer it by being sampled off a smaller copy of
    // itself, so the same ramp has to reach it as a rung of its blur chain.
    let color = wallpaper(uv, aspect, globals.time, soften, soften * SCENERY_LEVELS);

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
    // The rectangle this pane is cut to, as its two corners in pixels. A pane
    // nothing is cutting carries a box larger than any display.
    @location(8) cut: vec4<f32>,
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
    @location(9) cut: vec4<f32>,
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
    let below = wallpaper(uv, aspect, globals.time, soften, max(lod, soften * SCENERY_LEVELS));
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
