/*
 * The shell's wallpaper, on a web page.
 *
 * A port of `wallpaper()` in crates/lxb-desktop/src/shaders.wgsl — the band of
 * water, the glass-silk ribbons of the Simple theme, the sparkles the current
 * sheds, the ambient fields, the aurora veils and the vignette — from WGSL to
 * GLSL ES 3.00, kept line for line so it can be diffed against the original by
 * eye. The palette is the shell's own twelve accents from theme.rs, converted to
 * linear light exactly as `Color::rgb` converts them.
 *
 * While there is no glass to draw the scene goes straight to the canvas. When
 * there is — the start screen's tiles and the chosen row's disc, the guide's
 * column — it is drawn into a texture first, so each pane can be drawn as the
 * shell draws it: a slab of glass that bends what is behind it at its rim (the
 * glass branch of `fs_quad`).
 *
 * While the guide is open the screen is squeezed into a card beside it, as the
 * shell's is: the wallpaper drawn again inside the card as a miniature, and the
 * start screen's glass with it, over the same scene softened.
 */
(function () {
  "use strict";

  const PALETTES = {
    Purple: { accent: "8B5CF6", accent_soft: "C4B5FD", accent_deep: "4C1D95", glass: "140B26", glass_raised: "B9A7E8", text_soft: "D6CBEF", glow: "5B21B6", sky: ["1B1140", "060314", "2A1252", "0A0620"] },
    Blue: { accent: "3B82F6", accent_soft: "93C5FD", accent_deep: "1E3A8A", glass: "081026", glass_raised: "A7C6E8", text_soft: "CBDDEF", glow: "1E40AF", sky: ["0F1B40", "030614", "122A52", "060A20"] },
    Green: { accent: "16A34A", accent_soft: "91D39F", accent_deep: "14532D", glass: "06170D", glass_raised: "A7C8B1", text_soft: "C7DFCE", glow: "14532D", sky: ["082114", "020A05", "0A3018", "030F08"] },
    Yellow: { accent: "CA8A04", accent_soft: "DFBC73", accent_deep: "713F12", glass: "151002", glass_raised: "D8C389", text_soft: "E9DFC3", glow: "713F12", sky: ["292008", "0D0A02", "382B0A", "151003"] },
    Red: { accent: "EF4444", accent_soft: "FCA5A5", accent_deep: "7F1D1D", glass: "230707", glass_raised: "D8A7A7", text_soft: "E9CCCC", glow: "991B1B", sky: ["3A0D0D", "100202", "501010", "1E0505"] },
    Teal: { accent: "14B8A6", accent_soft: "5EEAD4", accent_deep: "134E4A", glass: "051A19", glass_raised: "A7CFC9", text_soft: "C7E0DB", glow: "115E59", sky: ["08302B", "020F0D", "0B4038", "041614"] },
    Indigo: { accent: "A5B4FC", accent_soft: "C7D2FE", accent_deep: "312E81", glass: "0D0E22", glass_raised: "B3B9DE", text_soft: "D5D9F2", glow: "252270", sky: ["101128", "030409", "15163A", "070813"] },
    Pink: { accent: "EC4899", accent_soft: "F9A8D4", accent_deep: "831843", glass: "200714", glass_raised: "E0A7C4", text_soft: "F0CCE0", glow: "861042", sky: ["2C0A1E", "0C0207", "3E0D28", "160410"] },
    Orange: { accent: "F97316", accent_soft: "FDBA74", accent_deep: "7C2D12", glass: "1D0B02", glass_raised: "E8C3A7", text_soft: "F0DAC4", glow: "842C0F", sky: ["301206", "0D0402", "421A07", "160702"] },
    White: { accent: "E4E4E4", accent_soft: "F5F5F5", accent_deep: "3D3D3D", glass: "0C0C0C", glass_raised: "C4C4C4", text_soft: "C9C9C9", glow: "444444", sky: ["151515", "040404", "1E1E1E", "090909"] },
    Silver: { accent: "A6A6A6", accent_soft: "D6D6D6", accent_deep: "4A4A4A", glass: "121212", glass_raised: "C0C0C0", text_soft: "CFCFCF", glow: "6B6B6B", sky: ["2A2A2A", "0B0B0B", "383838", "131313"] },
    Black: { accent: "525252", accent_soft: "8A8A8A", accent_deep: "1F1F1F", glass: "0A0A0A", glass_raised: "969696", text_soft: "B0B0B0", glow: "2E2E2E", sky: ["141414", "030303", "1C1C1C", "070707"] },
  };

  function linear(c) {
    return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
  }
  function hexLinear(hex) {
    const n = parseInt(hex, 16);
    return [linear(((n >> 16) & 255) / 255), linear(((n >> 8) & 255) / 255), linear((n & 255) / 255)];
  }
  function rendered(name) {
    const p = PALETTES[name] || PALETTES.Purple;
    return {
      accent: hexLinear(p.accent),
      accent_soft: hexLinear(p.accent_soft),
      accent_deep: hexLinear(p.accent_deep),
      glass: hexLinear(p.glass),
      glass_raised: hexLinear(p.glass_raised),
      glow: hexLinear(p.glow),
      sky: p.sky.map(hexLinear),
    };
  }
  function mixTheme(a, b, t) {
    const m = (x, y) => x.map((v, i) => v + (y[i] - v) * t);
    return {
      accent: m(a.accent, b.accent),
      accent_soft: m(a.accent_soft, b.accent_soft),
      accent_deep: m(a.accent_deep, b.accent_deep),
      glass: m(a.glass, b.glass),
      glass_raised: m(a.glass_raised, b.glass_raised),
      glow: m(a.glow, b.glow),
      sky: a.sky.map((s, i) => m(s, b.sky[i])),
    };
  }

  // ---------------------------------------------------------------------------
  // GLSL. Everything below `wallpaper()` is a transcription of shaders.wgsl.
  // WGSL's `select(a, b, c)` is `c ? b : a`, and `bitcast<u32>(i32(x))` is
  // `uint(int(x))`, which keeps the bit pattern of a negative row.
  // ---------------------------------------------------------------------------

  const VERTEX = `#version 300 es
precision highp float;
const vec2 P[3] = vec2[3](vec2(-1.0, -3.0), vec2(-1.0, 1.0), vec2(3.0, 1.0));
out vec2 v_uv;
void main() {
  vec2 p = P[gl_VertexID];
  gl_Position = vec4(p, 0.0, 1.0);
  v_uv = vec2((p.x + 1.0) * 0.5, (1.0 - p.y) * 0.5);
}`;

  const COMMON = `#version 300 es
precision highp float;
precision highp int;

uniform vec2 u_resolution;
uniform float u_time;
uniform vec4 u_sky[4];
uniform vec4 u_accent[3];
uniform vec4 u_glow;
uniform vec4 u_style;

const vec3 KEY_LIGHT = vec3(-0.42, -0.66, 0.62);
const float PI = 3.14159265;
const float BAND_FOLD = 0.020;
const float BAND_BOW = 0.50;
const float BAND_SHARE = 0.88;
const float BAND_EDGE_SAMPLES = 0.8;
const float GLASS_IOR = 1.47;
const float GLASS_DISPERSION = 0.055;
const float GLASS_FLOAT = 1.0;
const vec3 FROST_SCATTER = vec3(0.030, 0.029, 0.046);

float bevel_rise(float inset) {
  return sqrt(max(1.0 - (1.0 - inset) * (1.0 - inset), 0.0));
}
float bevel_slope(float inset) {
  return (1.0 - inset) / max(bevel_rise(inset), 0.16);
}
vec3 encode(vec3 c) {
  c = clamp(c, 0.0, 1.0);
  return mix(c * 12.92, 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, step(vec3(0.0031308), c));
}
vec3 decode(vec3 c) {
  return mix(c / 12.92, pow((c + 0.055) / 1.055, vec3(2.4)), step(vec3(0.04045), c));
}
// Half a step of eight-bit noise, so a dark gradient does not band.
float dither(vec2 p) {
  return (fract(sin(dot(p, vec2(12.9898, 78.233))) * 43758.5453) - 0.5) / 255.0;
}
`;

  const WALLPAPER = `
uniform float u_soften;
uniform vec4 u_glows[6];
uniform vec4 u_glow_colors[6];
uniform int u_glow_count;
uniform int u_glow_screen;
uniform vec4 u_card;
uniform vec4 u_card_shape;
uniform float u_encode;
in vec2 v_uv;
out vec4 frag;

float ambient_field(vec2 p, vec2 center, vec2 radius) {
  vec2 q = (p - center) / radius;
  return exp(-dot(q, q) * 1.65);
}

struct Spine { float gather; float height; float slope; };
const float SPINE_REST = 0.62;

Spine spine_at(float u, float t, float aspect) {
  float gather = 0.28 + 0.72 * sin(PI * u);
  float gather_slope = 0.72 * PI * cos(PI * u);
  float spine_a = u * 6.8 + t * 0.56;
  float spine_b = u * 3.4 - t * 0.39 + 0.8;
  float swing = sin(spine_a) * 0.055 + sin(spine_b) * 0.085;
  float swing_slope = cos(spine_a) * 0.055 * 6.8 + cos(spine_b) * 0.085 * 3.4;
  Spine spine;
  spine.gather = gather;
  spine.height = SPINE_REST + swing * gather;
  spine.slope = (swing_slope * gather + swing * gather_slope) / aspect;
  return spine;
}

vec3 water(vec3 into, vec2 uv, float aspect, float t, float soften, vec2 footprint, Spine spine) {
  vec3 color = into;
  float lip = 0.45;
  float ribbon_gloss = mix(1.0, 0.10, soften);
  float ribbon_wave = mix(1.0, 0.25, soften);
  vec3 key = normalize(KEY_LIGHT);
  vec3 half_vector = normalize(key + vec3(0.0, 0.0, 1.0));
  float gather = spine.gather;
  float slope = spine.slope;
  vec2 across = normalize(vec2(slope, -1.0));
  vec2 along = vec2(-across.y, across.x);
  float band = (uv.y - spine.height) / sqrt(1.0 + slope * slope);
  float sample_ = abs(across.x * footprint.x * aspect) + abs(across.y * footprint.y);

  float tilt[3];
  float width[3];
  for (int i = 0; i < 3; i++) {
    float fi = float(i);
    float x = uv.x * (2.0 + fi * 0.6);
    float turning = sin(x * 2.10 - t * (0.23 + fi * 0.05) + fi * 1.9) * 0.95
      + sin(x * 1.25 + t * 0.15 + fi * 2.7) * 0.55
      + sin(x * 4.30 - t * 0.35 + fi * 0.7) * 0.22;
    tilt[i] = (turning * abs(turning) * 0.62 + sin(x * 7.0 + t * 0.9 + fi) * 0.08) * ribbon_wave;
    float broad = sqrt(cos(tilt[i]) * cos(tilt[i]) + BAND_FOLD);
    width[i] = (0.0640 - 0.0110 * fi) * broad * mix(1.0, 1.5, soften);
  }

  float lift = BAND_SHARE * (0.32 + 0.68 * sin(uv.x * 4.3 - t * 0.37));
  float drop_ = BAND_SHARE * (0.32 + 0.68 * sin(uv.x * 3.1 + t * 0.29 + 2.2));
  float offset[3];
  offset[0] = -(width[0] + width[1]) * lift * gather;
  offset[1] = 0.0;
  offset[2] = (width[1] + width[2]) * drop_ * gather;

  for (int i = 0; i < 3; i++) {
    float fi = float(i);
    float x = uv.x * (2.0 + fi * 0.6);
    float d = band - offset[i];
    float sheet_width = width[i];
    float along_wave = (cos(x * 9.0 - t * 1.1 + fi * 2.0) * 0.34
      + cos(x * 23.0 + t * 1.9 + fi * 1.3) * 0.14) * ribbon_wave;
    float reach = abs(d / sheet_width);
    float s_across = clamp(d / sheet_width, -1.0, 1.0);
    float spread = BAND_EDGE_SAMPLES * sample_ / sheet_width;
    float cover = 1.0 - smoothstep(mix(0.94, 0.40, soften) - spread, 1.0 + spread, reach);
    float inset = clamp((1.0 - reach) / lip, 0.0, 1.0);
    float rise = bevel_rise(inset);
    vec2 wall = normalize(vec2(-sign(d) * bevel_slope(inset) - s_across * BAND_BOW, 1.0));
    float turn = sin(tilt[i]);
    float level = cos(tilt[i]);
    vec2 face = vec2(wall.x * level + wall.y * turn, wall.y * level - wall.x * turn);
    float broad = sqrt(level * level + BAND_FOLD);
    float fold = min(1.0 / broad, 2.2);
    vec3 surface = normalize(vec3(across * face.x + along * along_wave * face.y, face.y));
    float facing = clamp(dot(surface, key), 0.0, 1.0);
    float fresnel = 0.04 + 0.96 * pow(1.0 - clamp(surface.z, 0.0, 1.0), 5.0);
    float glint = pow(max(dot(surface, half_vector), 0.0), 42.0);
    vec3 mirrored = reflect(vec3(0.0, 0.0, -1.0), surface);
    float room = 0.42 + 0.73 * (0.5 - 0.5 * dot(mirrored, key));
    float travelling = 0.60 + 0.40 * sin(x * 3.1 - t * (0.9 + fi * 0.25) + fi);
    float focusing = 0.5 + 0.5 * sin(x * 2.3 - t * 0.5 + fi);
    float gathered = pow(max(sin(x * 23.0 + t * 1.9 + fi * 1.3 + s_across * 3.4), 0.0), 5.0)
      * (0.15 + 0.85 * focusing * focusing);
    float depth = 1.0 - fi * 0.26;
    float skirt = exp(-d * d * mix(90.0, 45.0, soften));
    float shadow_d = d - sheet_width - 0.010;
    float shadow = exp(-shadow_d * shadow_d * mix(1400.0, 500.0, soften)) * (1.0 - cover);
    float caustic_d = d - sheet_width - 0.0040;
    float caustic = exp(-caustic_d * caustic_d * mix(30000.0, 2000.0, soften)) * broad;
    vec3 haze_tint = mix(u_accent[0].rgb, u_accent[2].rgb, 0.46);
    vec3 body_tint = mix(u_accent[0].rgb, u_accent[2].rgb, 0.24 + fi * 0.05 + 0.36 * rise);
    float split = GLASS_DISPERSION * (1.0 - inset) * (1.0 - inset) * cover * depth * ribbon_gloss * -sign(d);
    float haze_strength = mix(1.0, 0.40, soften) * depth;
    float body_strength = mix(1.0, 0.55, soften) * depth;
    float water_ = cover * fold * body_strength;
    color *= 1.0 - cover * (0.05 + 0.11 * rise) * body_strength;
    color *= 1.0 - shadow * 0.20 * body_strength;
    color += haze_tint * skirt * 0.007 * haze_strength;
    color += body_tint * water_ * (0.005 + 0.012 * rise + 0.028 * facing);
    color += u_accent[1].rgb * fresnel * room * water_ * 0.100 * mix(1.0, 0.45, soften);
    color += u_accent[1].rgb * glint * water_ * 0.120 * travelling * ribbon_gloss;
    color += u_accent[1].rgb * gathered * cover * rise * 0.011 * depth * ribbon_gloss;
    color += u_accent[1].rgb * caustic * 0.011 * depth * ribbon_gloss;
    color *= vec3(1.0 + split, 1.0, 1.0 - split);
  }
  return color;
}

vec3 silk(vec3 into, vec2 uv, float aspect, float t, float soften) {
  vec3 color = into;
  for (int i = 0; i < 3; i++) {
    float fi = float(i);
    float speed = 0.42 + fi * 0.14;
    float lane = 0.62 + (fi - 1.0) * 0.050;
    float x_scale = 2.0 + fi * 0.6;
    float x = uv.x * x_scale;
    float phase_a = x * 2.6 + t * speed + fi * 2.1;
    float phase_b = x * 1.3 - t * speed * 0.7 + fi * 0.8;
    float center = lane + sin(phase_a) * 0.055 + sin(phase_b) * 0.085;
    float uv_slope = (cos(phase_a) * 0.055 * 2.6 + cos(phase_b) * 0.085 * 1.3) * x_scale;
    float slope = uv_slope / aspect;
    float d = (uv.y - center) / sqrt(1.0 + slope * slope);
    float skirt = exp(-d * d * mix(320.0, 110.0, soften));
    float body = exp(-d * d * mix(5200.0, 680.0, soften));
    float bevel_d = d + mix(0.0045, 0.012, soften);
    float bevel = exp(-bevel_d * bevel_d * mix(20000.0, 1000.0, soften));
    float crest_d = d + mix(0.0065, 0.014, soften);
    float crest = exp(-crest_d * crest_d * mix(70000.0, 1400.0, soften));
    float fold_d = d - mix(0.008, 0.016, soften);
    float lower_fold = exp(-fold_d * fold_d * mix(9500.0, 900.0, soften));
    vec2 upper_normal = normalize(vec2(slope, -1.0));
    float lamp_facing = max(dot(upper_normal, normalize(KEY_LIGHT.xy)), 0.0);
    float key_glint = 0.52 + 0.48 * pow(lamp_facing, 4.0);
    float travelling = 0.64 + 0.36 * sin(x * 3.1 - t * (0.9 + fi * 0.25) + fi);
    float depth = 1.0 - fi * 0.18;
    float haze_strength = mix(1.0, 0.40, soften) * depth;
    float gloss_strength = mix(1.0, 0.10, soften) * depth;
    vec3 haze_tint = mix(u_accent[0].rgb, u_accent[2].rgb, 0.46);
    vec3 body_tint = mix(u_accent[0].rgb, u_accent[2].rgb, 0.28 + fi * 0.04);
    color += haze_tint * skirt * 0.020 * haze_strength;
    color += body_tint * body * 0.040 * haze_strength;
    color += u_accent[2].rgb * lower_fold * 0.010 * haze_strength;
    color += u_accent[1].rgb * (bevel * 0.022 * (0.82 + 0.18 * travelling)
      + crest * 0.010 * travelling * key_glint) * gloss_strength;
  }
  return color;
}

Spine silk_spine(float u, float t, float aspect) {
  float speed = 0.56;
  float x_scale = 2.6;
  float x = u * x_scale;
  float phase_a = x * 2.6 + t * speed + 2.1;
  float phase_b = x * 1.3 - t * speed * 0.7 + 0.8;
  Spine spine;
  spine.gather = 1.0;
  spine.height = SPINE_REST + sin(phase_a) * 0.055 + sin(phase_b) * 0.085;
  spine.slope = (cos(phase_a) * 0.055 * 2.6 + cos(phase_b) * 0.085 * 1.3) * x_scale / aspect;
  return spine;
}

const float SPARKLE_LANE = 0.28;
const float SPARKLE_BIRTH = 0.03;
const float SPARKLE_SQUEEZE = 0.85;
const float SPARKLE_SINK = 0.6;
const float SPARKLE_STEEPEST = 0.95;

struct SparkleLayer {
  uint seed; vec2 cell; float drift; float rise; float density; float core;
  float reach; vec2 travel; float smallest; vec2 brightness; vec2 push; vec2 hold;
};

const SparkleLayer SPARKLE_DUST = SparkleLayer(
  0u, vec2(0.020, 0.023), 0.018, 0.016, 0.90, 0.0020, 0.0070,
  vec2(0.05, 0.14), 0.60, vec2(1.00, 0.15), vec2(0.03, 0.03), vec2(0.45, 0.15));
const SparkleLayer SPARKLE_GLINTS = SparkleLayer(
  1013904223u, vec2(0.075, 0.085), 0.022, 0.018, 0.60, 0.0048, 0.026,
  vec2(0.07, 0.18), 0.35, vec2(0.85, 0.22), vec2(0.06, 0.02), vec2(0.35, 0.15));

uint sparkle_hash(uint value) {
  uint state = value * 747796405u + 2891336453u;
  uint word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
  return (word >> 22u) ^ word;
}
float sparkle_unit(uint value) {
  return float(value >> 8u) / 16777216.0;
}
float sparkle_bits(uint value, uint shift, uint mask) {
  return float((value >> shift) & mask) / float(mask + 1u);
}
float sparkle_pushed(float drift, vec2 push) {
  return drift + push.x * drift / (push.y + abs(drift));
}
float sparkle_unpushed(float out_, vec2 push) {
  float d = abs(out_);
  float b = push.y + push.x - d;
  float root = sqrt(b * b + 4.0 * d * push.y);
  float drift = b > 0.0 ? 2.0 * d * push.y / (b + root) : 0.5 * (root - b);
  return sign(out_) * drift;
}
float sparkle_hold(float out_, vec2 hold) {
  float a = max(out_, 0.0);
  return (hold.y + hold.x * a) / (hold.y + a);
}
float sparkle_unheld(float offset, float held, vec2 hold) {
  if (offset <= held) {
    return offset - held;
  }
  float b = hold.y + held * hold.x - offset;
  float c = 4.0 * hold.y * (offset - held);
  float root = sqrt(b * b + c);
  return b > 0.0 ? 2.0 * hold.y * (offset - held) / (b + root) : 0.5 * (root - b);
}
float sparkle_bump(float x) {
  float q = max(1.0 - x * x, 0.0);
  return q * q * q;
}

vec2 sparkle_half(SparkleLayer layer, float side, float offset, float swing, float along,
                  float slope, float lane, float t, float sample_, float soften) {
  vec2 light = vec2(0.0);
  float reach_across = layer.reach * sqrt(1.0 + slope * slope);
  float rise = layer.rise * (side < 0.0 ? 1.0 : SPARKLE_SINK);
  uint half_seed = layer.seed ^ (side < 0.0 ? 0x9e3779b9u : 0u);
  float out_here = sparkle_unheld(side * offset, side * swing, layer.hold);
  float row_at = (sparkle_unpushed(out_here, layer.push) - rise * t) / layer.cell.y;
  float row_here = floor(row_at);
  float row_in = row_at - row_here;
  float row_side = row_in >= 0.5 ? 1.0 : -1.0;
  float row_gap = (row_in >= 0.5 ? 1.0 - row_in : row_in) * layer.cell.y;
  int rows = row_gap * SPARKLE_SQUEEZE < reach_across ? 2 : 1;
  for (int r = 0; r < rows; r++) {
    float row = row_here + float(r) * row_side;
    uint row_seed = sparkle_hash(uint(int(row)) + half_seed);
    float drifted = along - layer.drift * (0.6 + 0.8 * sparkle_unit(row_seed)) * t;
    float column_at = drifted / layer.cell.x;
    float column_here = floor(column_at);
    float column_in = column_at - column_here;
    float column_side = column_in >= 0.5 ? 1.0 : -1.0;
    float column_gap = (column_in >= 0.5 ? 1.0 - column_in : column_in) * layer.cell.x;
    int columns = column_gap < layer.reach ? 2 : 1;
    for (int c = 0; c < columns; c++) {
      float column = column_here + float(c) * column_side;
      uint cell_seed = sparkle_hash(row_seed + uint(int(column)));
      if (sparkle_unit(cell_seed) >= layer.density) {
        continue;
      }
      uint shape = sparkle_hash(cell_seed);
      uint look = sparkle_hash(shape);
      float phase = 2.0 * PI * sparkle_bits(look, 24u, 0xffu);
      float sway = sin(t * (0.12 + 0.20 * sparkle_bits(cell_seed, 0u, 0xffu)) + 2.0 * phase + 1.0) * 0.18;
      float d_along = (column + 0.5 + 0.60 * (sparkle_bits(shape, 0u, 0xffffu) - 0.5) + sway)
        * layer.cell.x - drifted;
      if (abs(d_along) >= layer.reach) {
        continue;
      }
      float strength = sparkle_bits(look, 0u, 0xffu);
      float grain = sparkle_bits(look, 8u, 0xffu);
      float pace = sparkle_bits(look, 16u, 0xffu);
      float wander = sin(t * (0.15 + 0.25 * pace) + phase) * 0.22;
      float out_ = sparkle_pushed((row + 0.5 + 0.56 * (sparkle_bits(shape, 16u, 0xffffu) - 0.5)
        + wander) * layer.cell.y + rise * t, layer.push);
      float hold = sparkle_hold(out_, layer.hold);
      float d_across = hold * swing + side * out_ - offset + hold * slope * d_along;
      float apart = sqrt(d_along * d_along + d_across * d_across);
      float size = mix(layer.smallest, 1.0, grain * grain);
      float reach = layer.reach * size;
      if (apart >= reach) {
        continue;
      }
      uint fate = sparkle_hash(look);
      float birth = SPARKLE_BIRTH * sparkle_bits(fate, 0u, 0xffffu);
      float travel = mix(layer.travel.x, layer.travel.y, sparkle_bits(fate, 16u, 0xffffu));
      float journey = out_ - birth;
      float left = clamp(1.0 - journey / travel, 0.0, 1.0);
      float life = smoothstep(0.0, 0.015, journey) * left * left * (3.0 - 2.0 * left);
      float twinkle = 0.75 + 0.25 * sin(t * (1.5 + 2.5 * grain) + phase);
      float fade = sparkle_bump(out_ / lane);
      float amount = (0.35 + 0.65 * strength * strength) * life * twinkle * fade * size;
      float radius = layer.core * size;
      float spread = min(sqrt(radius * radius + 2.25 * sample_ * sample_
        + 0.25 * reach * reach * soften * soften), reach);
      float kept = radius / spread;
      float fall = 1.0 - apart / reach;
      light += amount * vec2(sparkle_bump(apart / spread) * kept * kept, fall * fall * fall);
    }
  }
  return light;
}

vec2 sparkle_layer(SparkleLayer layer, float offset, float swing, float along, float slope,
                   float lane, float t, float sample_, float soften) {
  float side = offset >= swing ? 1.0 : -1.0;
  float reach_across = layer.reach * sqrt(1.0 + slope * slope);
  float lag = side * swing < 0.0 ? (1.0 - layer.hold.x) * abs(swing) : 0.0;
  if (abs(offset - swing) >= lane + lag + reach_across) {
    return vec2(0.0);
  }
  vec2 light = sparkle_half(layer, side, offset, swing, along, slope, lane, t, sample_, soften);
  if (abs(offset - swing) < reach_across) {
    light += sparkle_half(layer, -side, offset, swing, along, slope, lane, t, sample_, soften);
  }
  return light;
}

vec3 sparkles(vec3 into, vec2 uv, float aspect, float t, float soften, vec2 footprint, Spine spine) {
  float offset = uv.y - SPINE_REST;
  float swing = spine.height - SPINE_REST;
  float slope = clamp(spine.slope, -SPARKLE_STEEPEST, SPARKLE_STEEPEST);
  float lane = SPARKLE_LANE * (0.45 + 0.55 * spine.gather);
  bool behind = (offset - swing) * swing < 0.0;
  float lag = behind ? (1.0 - min(SPARKLE_DUST.hold.x, SPARKLE_GLINTS.hold.x)) * abs(swing) : 0.0;
  if (abs(offset - swing) >= lane + lag + SPARKLE_GLINTS.reach * sqrt(1.0 + slope * slope)) {
    return into;
  }
  float along = uv.x * aspect;
  float sample_ = max(footprint.x * aspect, footprint.y);
  vec2 dust = sparkle_layer(SPARKLE_DUST, offset, swing, along, slope, lane, t, sample_, soften);
  vec2 glints = sparkle_layer(SPARKLE_GLINTS, offset, swing, along, slope, lane, t, sample_, soften);
  float core = dust.x * SPARKLE_DUST.brightness.x + glints.x * SPARKLE_GLINTS.brightness.x;
  float glow = dust.y * SPARKLE_DUST.brightness.y + glints.y * SPARKLE_GLINTS.brightness.y;
  vec3 hot = mix(u_accent[1].rgb, vec3(1.0), 0.45);
  vec3 haze = mix(u_accent[0].rgb, u_accent[1].rgb, 0.5);
  return into + (hot * core + haze * glow) * mix(1.0, 0.5, soften);
}

vec3 over_the_wallpaper(vec3 into, vec2 uv, float soften) {
  vec3 color = into;
  float edge = distance(uv, vec2(0.5, 0.5));
  color *= 1.0 - smoothstep(0.55, 1.05, edge) * 0.55;
  return color * mix(1.0, 0.55, soften);
}

vec3 wallpaper(vec2 uv, float aspect, float t, float soften, vec2 footprint) {
  float mood = 0.5 + 0.5 * sin(t * 0.03);
  vec3 top = mix(u_sky[0].rgb, u_sky[2].rgb, mood);
  vec3 bottom = mix(u_sky[1].rgb, u_sky[3].rgb, mood);
  float gradient_y = uv.y + sin(uv.x * 2.7 + t * 0.075) * 0.045 + sin(uv.x * 5.3 - t * 0.052) * 0.018;
  vec3 color = mix(top, bottom, smoothstep(0.0, 1.0, gradient_y)) * 0.42;

  vec2 glow_center = vec2(0.24, 0.34);
  float glow = 1.0 - smoothstep(0.0, 0.8, distance(uv * vec2(aspect, 1.0), glow_center * vec2(aspect, 1.0)));
  color += u_glow.rgb * glow * 0.25;

  vec2 p = vec2((uv.x - 0.5) * aspect, uv.y - 0.5);
  vec2 warp = vec2(
    sin(p.y * 3.8 + t * 0.11) + sin((p.x + p.y) * 2.1 - t * 0.071),
    sin(p.x * 2.6 - t * 0.093) + sin((p.x - p.y) * 2.4 + t * 0.063)) * 0.035;
  vec2 flowed = p + warp;
  float spread = mix(1.0, 1.45, soften);
  float deep = ambient_field(flowed,
    vec2(-aspect * 0.34 + sin(t * 0.083) * aspect * 0.28, -0.23 + cos(t * 0.067) * 0.18),
    vec2(aspect * 0.36, 0.34) * spread);
  float main_ = ambient_field(flowed,
    vec2(aspect * 0.31 + cos(t * 0.061) * aspect * 0.30, 0.20 + sin(t * 0.089) * 0.20),
    vec2(aspect * 0.34, 0.38) * spread);
  float soft = ambient_field(flowed,
    vec2(sin(t * 0.047 + 2.0) * aspect * 0.42, sin(t * 0.073 + 1.1) * 0.30),
    vec2(aspect * 0.40, 0.29) * spread);
  float field_strength = mix(1.0, 0.48, soften);
  color += u_accent[2].rgb * deep * 0.16 * field_strength;
  color += u_accent[0].rgb * main_ * 0.085 * field_strength;
  color += u_accent[1].rgb * soft * 0.035 * field_strength;

  float current = sin(flowed.x * 2.15 + flowed.y * 1.25 + t * 0.13)
    + sin(flowed.x * 0.78 - flowed.y * 2.35 - t * 0.087);
  float current_light = smoothstep(0.32, 1.62, current);
  color += u_accent[0].rgb * current_light * 0.035 * mix(1.0, 0.40, soften);

  Spine spine = spine_at(uv.x, t, aspect);
  if (u_style.x > 0.5) {
    color = silk(color, uv, aspect, t, soften);
    spine = silk_spine(uv.x, t, aspect);
  } else {
    color = water(color, uv, aspect, t, soften, footprint, spine);
  }
  if (u_style.w > 0.5) {
    color = sparkles(color, uv, aspect, t, soften, footprint, spine);
  }

  float upper_center = 0.28 + sin(uv.x * 2.6 + t * 0.16) * 0.10 + sin(uv.x * 5.4 - t * 0.11) * 0.040;
  float upper_d = uv.y - upper_center;
  float upper_veil = exp(-upper_d * upper_d * mix(32.0, 13.0, soften));
  float upper_crest = exp(-upper_d * upper_d * mix(230.0, 55.0, soften));
  float upper_sheen = 0.72 + 0.28 * sin(uv.x * 4.2 - t * 0.22);
  float lower_center = 0.72 + sin(uv.x * 2.1 - t * 0.13 + 2.4) * 0.12 + sin(uv.x * 4.7 + t * 0.083) * 0.035;
  float lower_d = uv.y - lower_center;
  float lower_veil = exp(-lower_d * lower_d * mix(26.0, 11.0, soften));
  float lower_crest = exp(-lower_d * lower_d * mix(180.0, 45.0, soften));
  float lower_sheen = 0.74 + 0.26 * sin(uv.x * 3.7 + t * 0.18 + 1.7);
  float veil_strength = mix(1.0, 0.38, soften);
  color += u_accent[0].rgb * (upper_veil * 0.052 + upper_crest * 0.025) * upper_sheen * veil_strength;
  color += (u_accent[2].rgb * lower_veil * 0.16 + u_accent[0].rgb * lower_crest * 0.028)
    * lower_sheen * veil_strength;

  return over_the_wallpaper(color, uv, soften);
}

// A bloom — the shell's GLOW_SLOT, a gaussian with a hard fade ring — laid into
// the scene here, in linear light, so the glass drawn over it refracts it the
// way the shell's snapshot does. The slot is stretched over its quad like any
// other, so a glow that is not square is an ellipse.
vec3 glow_at(vec3 into, vec2 px, vec4 g, vec4 tint) {
  float r = length((px - g.xy) / max(g.zw * 0.5, vec2(1.0)));
  float falloff = exp(-5.5 * r * r) * clamp((1.0 - r) / 0.12, 0.0, 1.0);
  return mix(into, tint.rgb, clamp(tint.a * falloff, 0.0, 1.0));
}

// How much of the rounded rectangle \`rect\` (x, y, w, h) covers the pixel.
float rounded_coverage(vec2 px, vec4 rect, float radius) {
  vec2 half_ = rect.zw * 0.5;
  vec2 q = abs(px - rect.xy - half_) - half_ + vec2(min(radius, min(half_.x, half_.y)));
  float d = length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - min(radius, min(half_.x, half_.y));
  return 1.0 - smoothstep(-0.75, 0.75, d);
}

// The background pass in its two roles, as \`fs_background\` has them: the
// whole screen, or — while the guide is open — the screen squeezed into its
// card, a miniature of it rather than a crop, over the same scene softened.
// The first \`u_glow_screen\` glows belong to the screen and go into the card
// with it; the rest are laid over everything, where they were asked for.
void main() {
  vec2 px = v_uv * u_resolution;
  float aspect = u_resolution.x / max(u_resolution.y, 1.0);
  float inside = 1.0;
  vec2 uv = v_uv;
  vec2 footprint = 1.0 / u_resolution;
  if (u_card_shape.z > 0.5) {
    inside = rounded_coverage(px, u_card, u_card_shape.x);
    uv = (px - u_card.xy) / u_card.zw;
    footprint = 1.0 / u_card.zw;
  }
  vec3 color = vec3(0.0);
  if (inside < 1.0) {
    color = wallpaper(v_uv, aspect, u_time, u_soften, 1.0 / u_resolution);
  }
  if (inside > 0.0) {
    vec3 screen = wallpaper(uv, aspect, u_time, 0.0, footprint);
    vec2 at = uv * u_resolution;
    for (int i = 0; i < 6; i++) {
      if (i >= u_glow_screen) break;
      screen = glow_at(screen, at, u_glows[i], u_glow_colors[i]);
    }
    color = mix(color, screen, inside);
  }
  for (int i = 0; i < 6; i++) {
    if (i >= u_glow_count) break;
    if (i >= u_glow_screen) color = glow_at(color, px, u_glows[i], u_glow_colors[i]);
  }
  frag = vec4(encode(color) + dither(gl_FragCoord.xy + fract(u_time) * 17.0), 1.0);
}
`;

  // Blit the scene texture to the canvas. The texture already holds sRGB.
  const BLIT = `#version 300 es
precision highp float;
uniform sampler2D u_scene;
in vec2 v_uv;
out vec4 frag;
void main() {
  frag = vec4(texture(u_scene, vec2(v_uv.x, 1.0 - v_uv.y)).rgb, 1.0);
}`;

  // A pane of glass — the glass branch of `fs_quad`, for a filled pane with no
  // border and no notch. The quad is drawn in pixels with the origin at the top
  // left, like the shell's, and `behind()` reads the scene texture at the
  // blur-chain rung the frost asks for. `u_clip` is the shell's `cut`: what a
  // card's edge takes off a pane of the screen squeezed into it.
  const PANE_VERTEX = `#version 300 es
precision highp float;
uniform vec2 u_resolution;
uniform vec4 u_rect;
out vec2 v_local;
const vec2 C[6] = vec2[6](vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
                          vec2(1.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0));
void main() {
  vec2 corner = C[gl_VertexID];
  vec2 position = u_rect.xy + corner * u_rect.zw;
  gl_Position = vec4(position.x / u_resolution.x * 2.0 - 1.0, 1.0 - position.y / u_resolution.y * 2.0, 0.0, 1.0);
  v_local = corner * u_rect.zw;
}`;

  const PANE = `
uniform vec4 u_rect;
uniform vec4 u_color;
uniform vec4 u_shape;    // radius, corner power, slab depth, frost
uniform vec4 u_material; // gloss, fade, face curve
uniform vec4 u_clip;     // left, top, right, bottom
uniform sampler2D u_scene;
in vec2 v_local;
out vec4 frag;

float corner_norm(vec2 v, float power) {
  if (power <= 2.0) return length(v);
  return pow(pow(v.x, power) + pow(v.y, power), 1.0 / power);
}
float rounded_box(vec2 point, vec2 half_, float radius, float power) {
  float r = min(radius, min(half_.x, half_.y));
  vec2 q = abs(point) - half_ + vec2(r);
  return corner_norm(max(q, vec2(0.0)), power) + min(max(q.x, q.y), 0.0) - r;
}
vec2 edge_normal(vec2 point, vec2 half_, float radius, float power) {
  vec2 e = vec2(1.0, 0.0);
  vec2 gradient = vec2(
    rounded_box(point + e.xy, half_, radius, power) - rounded_box(point - e.xy, half_, radius, power),
    rounded_box(point + e.yx, half_, radius, power) - rounded_box(point - e.yx, half_, radius, power));
  float len = length(gradient);
  if (len < 0.0001) return vec2(0.0, -1.0);
  return gradient / len;
}
float bevel_shift(float inset, float slab, float eta) {
  float rise = bevel_rise(inset);
  vec3 normal = normalize(vec3(bevel_slope(inset), 0.0, 1.0));
  vec3 inside = refract(vec3(0.0, 0.0, -1.0), normal, eta);
  float shift = inside.x * (slab * rise / max(-inside.z, 0.05));
  float sin_in = abs(inside.x);
  if (sin_in > 1e-5) {
    float sin_out = min(sin_in / eta, 0.90);
    float cos_out = sqrt(max(1.0 - sin_out * sin_out, 1e-4));
    shift += sign(inside.x) * (sin_out / cos_out) * slab * GLASS_FLOAT;
  }
  return shift;
}
vec3 behind(vec2 px, float lod) {
  vec2 uv = clamp(px / u_resolution, vec2(0.0), vec2(1.0));
  return decode(textureLod(u_scene, vec2(uv.x, 1.0 - uv.y), lod).rgb);
}
vec3 environment(vec3 mirrored, float key_strength) {
  float sky = clamp(-mirrored.y, 0.0, 1.0);
  vec3 ambient = mix(vec3(0.03, 0.03, 0.05), vec3(1.20, 1.16, 1.45), sky * sky);
  float key = pow(max(dot(mirrored, normalize(KEY_LIGHT)), 0.0), 36.0);
  return ambient + vec3(1.0, 0.98, 0.93) * key * 26.0 * key_strength;
}

void main() {
  vec2 half_ = u_rect.zw * 0.5;
  vec2 point = v_local - half_;
  float radius = u_shape.x;
  float power = u_shape.y;
  float d = rounded_box(point, half_, radius, power);
  float coverage = 1.0 - smoothstep(-0.75, 0.75, d);
  if (coverage <= 0.0) discard;
  vec2 px = vec2(gl_FragCoord.x, u_resolution.y - gl_FragCoord.y);
  if (px.x < u_clip.x || px.y < u_clip.y || px.x > u_clip.z || px.y > u_clip.w) discard;

  float thickness = u_shape.z;
  float frost = u_shape.w;
  float gloss = u_material.x;
  float face_curve = u_material.z;
  float slab = min(thickness, min(half_.x, half_.y) * 0.44);
  float inset = slab > 0.0 ? clamp(-d / slab, 0.0, 1.0) : 1.0;
  float rise = bevel_rise(inset);
  vec2 outward = edge_normal(point, half_, radius, power);
  // A broad sheet — the guide's column — carries a very shallow bow, so the
  // room reflected in it changes across more than its rim. Eased out at the
  // lip, where the bevel is already the surface.
  vec2 face_slope = point / max(half_, vec2(1.0)) * vec2(0.28, 0.45) * face_curve * rise;
  vec3 surface = normalize(vec3(outward * bevel_slope(inset) + face_slope, 1.0));

  vec3 glass = u_color.rgb;
  float alpha = u_color.a;
  if (slab > 0.0) {
    float lod = frost * 4.0 * (0.3 + 0.7 * inset);
    float eta = 1.0 / GLASS_IOR;
    float spread = GLASS_DISPERSION * eta;
    float shift = bevel_shift(inset, slab, eta);
    vec3 red = behind(px + outward * bevel_shift(inset, slab, eta + spread), lod);
    vec3 green = behind(px + outward * shift, lod);
    vec3 blue = behind(px + outward * bevel_shift(inset, slab, eta - spread), lod);
    float step_ = 0.02;
    float sample_moves = (bevel_shift(inset + step_, slab, eta) - shift) / (step_ * slab);
    float caustic = clamp(1.0 / max(abs(1.0 - sample_moves), 0.3), 0.45, 2.6);
    float stain = u_color.a * mix(0.4, 1.0, rise);
    glass = mix(vec3(red.r, green.g, blue.b) * caustic, u_color.rgb, stain);
    glass += FROST_SCATTER * frost;
    alpha = 1.0;
  }
  if (gloss > 0.0) {
    float fresnel = 0.04 + 0.96 * pow(1.0 - clamp(surface.z, 0.0, 1.0), 5.0);
    float lit = fresnel * gloss;
    // The lamp at full strength on the true bevel, and only a sheen across
    // a curved face, or it reads as a white decal laid over the panel.
    float broad_face = smoothstep(0.68, 1.0, inset) * clamp(face_curve, 0.0, 1.0);
    glass = mix(glass, environment(reflect(vec3(0.0, 0.0, -1.0), surface), mix(1.0, 0.10, broad_face)), lit);
    alpha = max(alpha, lit);
  }
  frag = vec4(encode(max(glass, vec3(0.0))), alpha * coverage * u_material.y);
}
`;

  function compile(gl, type, source) {
    const shader = gl.createShader(type);
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) {
      const log = gl.getShaderInfoLog(shader);
      gl.deleteShader(shader);
      throw new Error("shader: " + log);
    }
    return shader;
  }
  function program(gl, vertex, fragment) {
    const p = gl.createProgram();
    gl.attachShader(p, compile(gl, gl.VERTEX_SHADER, vertex));
    gl.attachShader(p, compile(gl, gl.FRAGMENT_SHADER, fragment));
    gl.linkProgram(p);
    if (!gl.getProgramParameter(p, gl.LINK_STATUS)) {
      throw new Error("link: " + gl.getProgramInfoLog(p));
    }
    const uniforms = {};
    const count = gl.getProgramParameter(p, gl.ACTIVE_UNIFORMS);
    for (let i = 0; i < count; i++) {
      const info = gl.getActiveUniform(p, i);
      const name = info.name.replace(/\[0\]$/, "");
      uniforms[name] = gl.getUniformLocation(p, info.name);
    }
    return { p, u: uniforms };
  }

  /**
   * The scene behind a page.
   *
   * `panes` and `glows` belong to the screen — the start screen's glass and
   * the bloom behind the chosen tile — in the screen's own pixels, and go into
   * the guide's card with it. `card` is where the screen is while the guide is
   * open (x, y, w, h and the radius of its corners, in CSS pixels), and
   * `overlay` is what the guide lays over everything: its column of glass and
   * the light beneath it.
   *
   * `options.fps` holds a page to fewer frames a second than the display's;
   * `hurry()` lifts that for a moment, while something is flying.
   */
  class Wallpaper {
    constructor(canvas, options) {
      this.canvas = canvas;
      this.fps = (options && options.fps) || 0;
      this.fastUntil = 0;
      this.reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
      this.style = { simple: false, particles: true };
      this.soften = 0;
      this.softenTarget = 0;
      this.glows = [];
      this.panes = [];
      this.card = null;
      this.overlay = { panes: [], glows: [] };
      this.hooks = [];
      this.quality = 1;
      this.slow = 0;
      this.frames = 0;
      this.last = 0;
      this.theme = rendered("Purple");
      this.themeFrom = this.theme;
      this.themeTo = this.theme;
      this.themeAt = 1;
      this.startedAt = Wallpaper.clockStart();
      const gl = canvas.getContext("webgl2", { alpha: false, antialias: false, depth: false, stencil: false, premultipliedAlpha: false, powerPreference: "default" });
      if (!gl) throw new Error("no WebGL2");
      this.gl = gl;
      this.scene = program(gl, VERTEX, COMMON + WALLPAPER);
      if (options && options.glass) this.ensureGlass();
      this.vao = gl.createVertexArray();
      gl.bindVertexArray(this.vao);
      canvas.addEventListener("webglcontextlost", (e) => { e.preventDefault(); this.lost = true; });
      this.resize();
      new ResizeObserver(() => { this.resize(); this.draw(true); }).observe(canvas);
      document.addEventListener("visibilitychange", () => { if (!document.hidden) this.kick(); });
      this.kick();
    }

    /**
     * Where the wallpaper's clock started, carried from page to page for the
     * length of a visit — the web's version of the hand-over record CEDM gives
     * the shell, so going from one page to the next is one continuous picture
     * rather than the scene starting over.
     */
    static clockStart() {
      const now = Date.now();
      let start = now - 20000;
      try {
        const kept = Number(sessionStorage.getItem("lxb-wallpaper-clock"));
        if (kept && now - kept < 6 * 3600 * 1000) start = kept;
        else sessionStorage.setItem("lxb-wallpaper-clock", String(start));
      } catch (e) { /* storage unavailable: start where we are */ }
      return start;
    }

    resize() {
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      const rect = this.canvas.getBoundingClientRect();
      const cssW = Math.max(1, rect.width), cssH = Math.max(1, rect.height);
      // The scene is smooth everywhere but at the edges of the ribbons, so it
      // is drawn at no more than about two million pixels and let the browser
      // scale it; a slow machine draws it at less, as the shell's low-end mode
      // does, rather than dropping frames.
      let scale = dpr * this.quality;
      const budget = 2.2e6;
      if (cssW * cssH * scale * scale > budget) scale = Math.sqrt(budget / (cssW * cssH));
      this.scale = scale;
      this.cssW = cssW;
      this.cssH = cssH;
      const w = Math.max(1, Math.round(cssW * scale));
      const h = Math.max(1, Math.round(cssH * scale));
      if (this.canvas.width !== w || this.canvas.height !== h) {
        this.canvas.width = w;
        this.canvas.height = h;
        if (this.texture) this.target(w, h);
      }
    }

    // The two programs glass needs, made the first time there is glass to draw.
    ensureGlass() {
      if (this.pane) return;
      this.blit = program(this.gl, VERTEX, BLIT);
      this.pane = program(this.gl, PANE_VERTEX, COMMON + PANE);
    }

    target(w, h) {
      const gl = this.gl;
      if (this.texture) gl.deleteTexture(this.texture);
      if (this.fbo) gl.deleteFramebuffer(this.fbo);
      this.texture = gl.createTexture();
      gl.bindTexture(gl.TEXTURE_2D, this.texture);
      const levels = Math.floor(Math.log2(Math.max(w, h))) + 1;
      gl.texStorage2D(gl.TEXTURE_2D, levels, gl.RGBA8, w, h);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR_MIPMAP_LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
      this.fbo = gl.createFramebuffer();
      gl.bindFramebuffer(gl.FRAMEBUFFER, this.fbo);
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, this.texture, 0);
      gl.bindFramebuffer(gl.FRAMEBUFFER, null);
    }

    /** Change the accent, travelling there the way the shell does. */
    setAccent(name, animate) {
      this.themeFrom = this.theme;
      this.themeTo = rendered(name);
      this.themeAt = animate === false ? 1 : 0;
      if (!animate) this.theme = this.themeTo;
      this.kick();
    }
    setStyle(style) {
      Object.assign(this.style, style);
      this.kick();
    }
    /** Softened and dimmed, as the shell draws its backdrop under the guide. */
    setSoften(value) {
      this.softenTarget = value;
      this.kick();
    }
    /** The same, set outright by something already easing it frame by frame. */
    setSoftenNow(value) {
      this.soften = this.softenTarget = value;
    }
    /** Every frame the display has, for the next `seconds`. */
    hurry(seconds) {
      this.fastUntil = Math.max(this.fastUntil, performance.now() + seconds * 1000);
      this.kick();
    }

    time() {
      return (Date.now() - this.startedAt) / 1000;
    }

    kick() {
      if (this.pending) return;
      this.pending = requestAnimationFrame((now) => this.frame(now));
    }

    frame(now) {
      this.pending = 0;
      if (document.hidden || this.lost) return;
      const dt = this.last ? Math.min((now - this.last) / 1000, 0.1) : 0.016;
      if (this.fps && !this.reduced && this.last && now - this.last < 1000 / this.fps - 2 && now > this.fastUntil) {
        this.kick();
        return;
      }
      this.last = now;
      if (this.themeAt < 1) {
        this.themeAt = Math.min(1, this.themeAt + dt / 0.45);
        const e = this.themeAt < 0.5 ? 4 * this.themeAt ** 3 : 1 - (-2 * this.themeAt + 2) ** 3 / 2;
        this.theme = mixTheme(this.themeFrom, this.themeTo, e);
      }
      if (this.soften !== this.softenTarget) {
        const step = dt / 0.28;
        this.soften = this.soften < this.softenTarget
          ? Math.min(this.softenTarget, this.soften + step)
          : Math.max(this.softenTarget, this.soften - step);
      }
      // A hook that returns false has nothing left to move; any other answer,
      // or none, asks for the next frame.
      let wanted = false;
      for (const hook of this.hooks.slice()) if (hook(dt, now) !== false) wanted = true;
      const moving = !this.reduced || this.themeAt < 1 || this.soften !== this.softenTarget || wanted;
      const started = performance.now();
      this.draw();
      this.measure(performance.now() - started, dt);
      if (moving) this.kick();
    }

    // A machine that cannot keep up is drawn at a lower resolution rather than
    // at a lower rate. Judged over a second of frames, never on one.
    measure(spent, dt) {
      this.frames++;
      if (dt > (this.fps ? 1.6 / this.fps : 0.034)) this.slow++;
      if (this.frames >= 60) {
        if (this.slow > 30 && this.quality > 0.5) {
          this.quality = Math.max(0.5, this.quality * 0.8);
          this.resize();
        }
        this.frames = 0;
        this.slow = 0;
      }
    }

    uniforms(prog) {
      const gl = this.gl, u = prog.u, th = this.theme;
      gl.uniform2f(u.u_resolution, this.canvas.width, this.canvas.height);
      if (u.u_time) gl.uniform1f(u.u_time, this.reduced ? 40 : this.time());
      if (u.u_sky) gl.uniform4fv(u.u_sky, th.sky.flatMap((c) => [c[0], c[1], c[2], 1]));
      if (u.u_accent) gl.uniform4fv(u.u_accent, [...th.accent, 1, ...th.accent_soft, 1, ...th.accent_deep, 1]);
      if (u.u_glow) gl.uniform4f(u.u_glow, th.glow[0], th.glow[1], th.glow[2], 1);
      if (u.u_style) gl.uniform4f(u.u_style, this.style.simple ? 1 : 0, 0, 0, this.style.particles ? 1 : 0);
    }

    draw() {
      const gl = this.gl;
      if (!gl || this.lost) return;
      const W = this.canvas.width, H = this.canvas.height, s = this.scale;
      const card = this.card;

      // The screen's own glass goes into the card with it, miniaturised rather
      // than switched off, and is cut at the card's edge as the display's edge
      // cut it: what the screen never showed is not shown in the card either.
      const panes = [];
      const k = card ? card.w / this.cssW : 1;
      for (const q of this.panes) {
        if (q.fade <= 0.003) continue;
        panes.push(card ? {
          ...q,
          x: card.x + q.x * k, y: card.y + q.y * k, w: q.w * k, h: q.h * k,
          radius: q.radius * k, slab: q.slab * k,
          clip: [card.x, card.y, card.x + card.w, card.y + card.h],
        } : q);
      }
      for (const q of this.overlay.panes) if (q.fade > 0.003) panes.push(q);
      const glass = panes.length > 0;
      if (glass) {
        this.ensureGlass();
        if (!this.texture) this.target(W, H);
      }

      gl.bindVertexArray(this.vao);
      gl.disable(gl.BLEND);
      gl.bindFramebuffer(gl.FRAMEBUFFER, glass ? this.fbo : null);
      gl.viewport(0, 0, W, H);
      gl.useProgram(this.scene.p);
      const u = this.scene.u;
      this.uniforms(this.scene);
      gl.uniform1f(u.u_soften, this.soften);
      if (card) {
        gl.uniform4f(u.u_card, card.x * s, card.y * s, card.w * s, card.h * s);
        gl.uniform4f(u.u_card_shape, card.radius * s, 0, 1, 0);
      } else {
        gl.uniform4f(u.u_card_shape, 0, 0, 0, 0);
      }
      const screenGlows = this.glows.slice(0, 4);
      const glows = screenGlows.concat(this.overlay.glows).slice(0, 6);
      gl.uniform1i(u.u_glow_count, glows.length);
      gl.uniform1i(u.u_glow_screen, screenGlows.length);
      if (glows.length) {
        const pos = [], col = [];
        for (const g of glows) {
          pos.push(g.x * s, g.y * s, (g.w || g.size) * s, (g.h || g.size) * s);
          col.push(...g.color);
        }
        while (pos.length < 24) { pos.push(0); col.push(0); }
        gl.uniform4fv(u.u_glows, pos);
        gl.uniform4fv(u.u_glow_colors, col);
      }
      gl.drawArrays(gl.TRIANGLES, 0, 3);
      if (!glass) return;

      gl.bindTexture(gl.TEXTURE_2D, this.texture);
      gl.generateMipmap(gl.TEXTURE_2D);
      gl.bindFramebuffer(gl.FRAMEBUFFER, null);
      gl.viewport(0, 0, W, H);
      gl.useProgram(this.blit.p);
      gl.uniform1i(this.blit.u.u_scene, 0);
      gl.drawArrays(gl.TRIANGLES, 0, 3);

      gl.enable(gl.BLEND);
      gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA);
      gl.useProgram(this.pane.p);
      this.uniforms(this.pane);
      gl.uniform1i(this.pane.u.u_scene, 0);
      for (const q of panes) {
        const clip = q.clip || [-1e6, -1e6, 1e6, 1e6];
        gl.uniform4f(this.pane.u.u_rect, q.x * s, q.y * s, q.w * s, q.h * s);
        gl.uniform4fv(this.pane.u.u_color, q.color);
        gl.uniform4f(this.pane.u.u_shape, q.radius * s, q.power || 2, q.slab * s, q.frost);
        gl.uniform4f(this.pane.u.u_material, q.gloss, q.fade, q.curve || 0, 0);
        gl.uniform4f(this.pane.u.u_clip, clip[0] * s, clip[1] * s, clip[2] * s, clip[3] * s);
        gl.drawArrays(gl.TRIANGLES, 0, 6);
      }
    }
  }

  window.LXB = window.LXB || {};
  window.LXB.PALETTES = PALETTES;
  window.LXB.rendered = rendered;
  window.LXB.Wallpaper = Wallpaper;
})();
