// The two passes that happen around the frame rather than in it: the blur
// chain the glass reads what is behind it through, and the copy that puts the
// finished frame on the display.
//
// Their own module because they bind a texture at the slot the shell's own
// shaders bind the globals to, and one module cannot declare both.

@group(0) @binding(0) var source_texture: texture_2d<f32>;
@group(0) @binding(1) var source_sampler: sampler;

struct FullscreenOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> FullscreenOut {
    // One oversized triangle; the third vertex lies outside the viewport.
    var positions = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -3.0),
        vec2<f32>(-1.0, 1.0),
        vec2<f32>(3.0, 1.0),
    );
    let p = positions[index];

    var out: FullscreenOut;
    out.clip = vec4<f32>(p, 0.0, 1.0);
    out.uv = vec2<f32>((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
    return out;
}

// One rung of the blur chain: half the size of the rung above it, gathered
// with five taps whose four outer ones sit on texel *corners*, so bilinear
// filtering turns each of them into four. Run down the chain that converges on
// a gaussian, which is what frost has to be — a box blur repeated stays a box,
// and once the radius is wide enough to matter its own square edges show.
//
// The source is bound as a single-level view, so the dimensions asked for here
// are that rung's rather than the whole chain's.
@fragment
fn fs_downsample(in: FullscreenOut) -> @location(0) vec4<f32> {
    let texel = 1.0 / vec2<f32>(textureDimensions(source_texture, 0));

    var sum = textureSampleLevel(source_texture, source_sampler, in.uv, 0.0) * 4.0;
    sum += textureSampleLevel(source_texture, source_sampler, in.uv + texel, 0.0);
    sum += textureSampleLevel(source_texture, source_sampler, in.uv - texel, 0.0);
    sum += textureSampleLevel(
        source_texture, source_sampler, in.uv + vec2<f32>(texel.x, -texel.y), 0.0);
    sum += textureSampleLevel(
        source_texture, source_sampler, in.uv + vec2<f32>(-texel.x, texel.y), 0.0);
    return sum / 8.0;
}

@fragment
fn fs_blit(in: FullscreenOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(source_texture, source_sampler, in.uv, 0.0);
}
