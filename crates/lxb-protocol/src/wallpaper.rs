//! The analytic wallpaper, as a function rather than as a picture.
//!
//! The shell draws this on the GPU, in `lxb-desktop`'s `shaders.wgsl`. The
//! compositor needs the same image before the shell exists — the seconds
//! between taking the displays and the shell's first frame used to be a black
//! screen — and it has no way to run that shader: its renderer is Smithay's
//! GLES one, and on the DRM backend it is a multi-GPU renderer on top of that.
//!
//! So the wallpaper lives here, in the crate both sides already share, as
//! plain arithmetic. It is deliberately the *same* arithmetic: the shader is
//! the authority and this is a transcription of it. `lxb-desktop`'s
//! `theme::tests::the_compositor_draws_the_same_palette_before_the_shell_starts`
//! fails if the two palettes ever part company.
//!
//! Nothing here is a Wayland protocol, but it is the same kind of thing as
//! one: a definition two processes have to agree on exactly, kept in one place
//! so they cannot disagree quietly.

/// A colour as authored: `0xRRGGBB` in sRGB.
///
/// The spelling `lxb-desktop::theme::Color` uses, so the two palette tables
/// can be compared without converting either of them first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    /// Linear RGB, the space every calculation below works in.
    pub fn rgb(self) -> [f32; 3] {
        [
            srgb_to_linear((self.0 >> 16 & 0xff) as f32 / 255.0),
            srgb_to_linear((self.0 >> 8 & 0xff) as f32 / 255.0),
            srgb_to_linear((self.0 & 0xff) as f32 / 255.0),
        ]
    }
}

/// The sRGB transfer function, inverted.
fn srgb_to_linear(channel: f32) -> f32 {
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

/// The sRGB transfer function.
///
/// The wallpaper is calculated in linear light, exactly as the shader
/// calculates it, and the shell's surface is an sRGB render target that
/// applies this on the way out. A compositor writing eight-bit pixels into a
/// plain `Abgr8888` buffer has to apply it itself, or its wallpaper is
/// visibly darker than the shell's at the moment the two swap over.
pub fn linear_to_srgb(channel: f32) -> f32 {
    if channel <= 0.003_130_8 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    }
}

/// The colours of one accent that the wallpaper is made of.
///
/// A subset of `lxb-desktop::theme::Theme`: the parts the *background* is
/// drawn from. Glass, text and the rest of the shell's palette are not here,
/// because nothing draws them before the shell is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// The name the user picks it by, and the value in `shell.toml`.
    pub name: &'static str,
    /// Gradient top and bottom, then the pair the mood drifts towards.
    pub sky: [Color; 4],
    /// The accent at its normal, soft and deep rungs.
    pub accent: [Color; 3],
    /// The glow behind the cross point.
    pub glow: Color,
}

/// Every accent the wallpaper can be drawn in, in the order Settings lists
/// them. The first is the default, which is what an unreadable or
/// unrecognised setting falls back to.
pub const PALETTES: &[Palette] = &[
    Palette {
        name: "Purple",
        sky: [
            Color(0x1B1140),
            Color(0x060314),
            Color(0x2A1252),
            Color(0x0A0620),
        ],
        accent: [Color(0x8B5CF6), Color(0xC4B5FD), Color(0x4C1D95)],
        glow: Color(0x5B21B6),
    },
    Palette {
        name: "Blue",
        sky: [
            Color(0x0F1B40),
            Color(0x030614),
            Color(0x122A52),
            Color(0x060A20),
        ],
        accent: [Color(0x3B82F6), Color(0x93C5FD), Color(0x1E3A8A)],
        glow: Color(0x1E40AF),
    },
    Palette {
        name: "Green",
        sky: [
            Color(0x082114),
            Color(0x020A05),
            Color(0x0A3018),
            Color(0x030F08),
        ],
        accent: [Color(0x16A34A), Color(0x91D39F), Color(0x14532D)],
        glow: Color(0x14532D),
    },
    Palette {
        name: "Yellow",
        sky: [
            Color(0x292008),
            Color(0x0D0A02),
            Color(0x382B0A),
            Color(0x151003),
        ],
        accent: [Color(0xCA8A04), Color(0xDFBC73), Color(0x713F12)],
        glow: Color(0x713F12),
    },
    Palette {
        name: "Red",
        sky: [
            Color(0x3A0D0D),
            Color(0x100202),
            Color(0x501010),
            Color(0x1E0505),
        ],
        accent: [Color(0xEF4444), Color(0xFCA5A5), Color(0x7F1D1D)],
        glow: Color(0x991B1B),
    },
];

/// The palette saved under `name`, or the default when it is not one of them.
///
/// Exact, case-sensitive matching, the same rule the shell and the display
/// manager both apply: the five names are the whole of the setting's domain,
/// and quietly accepting `blue` would make two spellings of one value.
pub fn palette(name: &str) -> &'static Palette {
    PALETTES
        .iter()
        .find(|palette| palette.name == name)
        .unwrap_or(&PALETTES[0])
}

/// One accent's colours resolved into linear light, once per image rather
/// than once per pixel.
#[derive(Debug, Clone, Copy)]
pub struct Sky {
    sky: [[f32; 3]; 4],
    accent: [[f32; 3]; 3],
    glow: [f32; 3],
}

impl Sky {
    pub fn new(palette: &Palette) -> Self {
        Self {
            sky: [
                palette.sky[0].rgb(),
                palette.sky[1].rgb(),
                palette.sky[2].rgb(),
                palette.sky[3].rgb(),
            ],
            accent: [
                palette.accent[0].rgb(),
                palette.accent[1].rgb(),
                palette.accent[2].rgb(),
            ],
            glow: palette.glow.rgb(),
        }
    }
}

/// The picture behind the shell at `uv`, in linear light.
///
/// `uv` runs 0..1 across and down the display, `aspect` is width over height,
/// and `t` is the wallpaper clock in seconds — the one the display manager
/// hands over, so the same `t` here and in the shell is the same frame.
///
/// This is `wallpaper()` from `shaders.wgsl` with `soften` and `lod` at zero
/// and no key art: the softened variants are for the guide's backdrop and the
/// glass panes, and there is no artwork to sample before the shell has read
/// the user's library. Keep the arithmetic in step with that function, term
/// for term, and bump the visual identity in the handoff record when it
/// changes in a way that would draw a different frame at the same `t`.
pub fn sample(sky: &Sky, uv: [f32; 2], aspect: f32, t: f32) -> [f32; 3] {
    let (u, v) = (uv[0], uv[1]);

    // The mood drifts slowly between the theme's two gradients.
    let mood = 0.5 + 0.5 * (t * 0.03).sin();
    let top = mix3(sky.sky[0], sky.sky[2], mood);
    let bottom = mix3(sky.sky[1], sky.sky[3], mood);
    // Even the base gradient moves: two very broad currents bend its horizon
    // in opposite directions.
    let gradient_y = v + (u * 2.7 + t * 0.075).sin() * 0.045 + (u * 5.3 - t * 0.052).sin() * 0.018;
    let mut color = scale3(mix3(top, bottom, smoothstep(0.0, 1.0, gradient_y)), 0.42);

    // A soft glow behind the cross point keeps the bar area readable.
    let glow_center = [0.24_f32, 0.34_f32];
    let glow = 1.0
        - smoothstep(
            0.0,
            0.8,
            distance([u * aspect, v], [glow_center[0] * aspect, glow_center[1]]),
        );
    color = add3(color, scale3(sky.glow, glow * 0.25));

    // A mesh of three display-sized light fields, warped so their edges flow
    // instead of exposing the ellipses used to calculate them.
    let p = [(u - 0.5) * aspect, v - 0.5];
    let warp = [
        ((p[1] * 3.8 + t * 0.11).sin() + ((p[0] + p[1]) * 2.1 - t * 0.071).sin()) * 0.035,
        ((p[0] * 2.6 - t * 0.093).sin() + ((p[0] - p[1]) * 2.4 + t * 0.063).sin()) * 0.035,
    ];
    let flowed = [p[0] + warp[0], p[1] + warp[1]];

    let deep = ambient_field(
        flowed,
        [
            -aspect * 0.34 + (t * 0.083).sin() * aspect * 0.28,
            -0.23 + (t * 0.067).cos() * 0.18,
        ],
        [aspect * 0.36, 0.34],
    );
    let main = ambient_field(
        flowed,
        [
            aspect * 0.31 + (t * 0.061).cos() * aspect * 0.30,
            0.20 + (t * 0.089).sin() * 0.20,
        ],
        [aspect * 0.34, 0.38],
    );
    let soft = ambient_field(
        flowed,
        [
            (t * 0.047 + 2.0).sin() * aspect * 0.42,
            (t * 0.073 + 1.1).sin() * 0.30,
        ],
        [aspect * 0.40, 0.29],
    );
    color = add3(color, scale3(sky.accent[2], deep * 0.16));
    color = add3(color, scale3(sky.accent[0], main * 0.085));
    color = add3(color, scale3(sky.accent[1], soft * 0.035));

    // One broad diagonal current puts visible motion between those pools.
    let current = (flowed[0] * 2.15 + flowed[1] * 1.25 + t * 0.13).sin()
        + (flowed[0] * 0.78 - flowed[1] * 2.35 - t * 0.087).sin();
    let current_light = smoothstep(0.32, 1.62, current);
    color = add3(color, scale3(sky.accent[0], current_light * 0.035));

    // The XMB current: three fine glass-silk ribbons moving together through
    // a broad lane below the cross point.
    let key_light = normalize2([-0.42, -0.66]);
    for i in 0..3 {
        let fi = i as f32;
        let speed = 0.42 + fi * 0.14;
        let lane = 0.62 + (fi - 1.0) * 0.050;
        let x_scale = 2.0 + fi * 0.6;
        let x = u * x_scale;

        let phase_a = x * 2.6 + t * speed + fi * 2.1;
        let phase_b = x * 1.3 - t * speed * 0.7 + fi * 0.8;
        let center = lane + phase_a.sin() * 0.055 + phase_b.sin() * 0.085;

        // Measured across the curve rather than vertically, so a steep
        // section does not grow visibly thicker than a flat one.
        let uv_slope = (phase_a.cos() * 0.055 * 2.6 + phase_b.cos() * 0.085 * 1.3) * x_scale;
        let slope = uv_slope / aspect;
        let d = (v - center) / (1.0 + slope * slope).sqrt();

        let skirt = (-d * d * 320.0).exp();
        let body = (-d * d * 5200.0).exp();
        let bevel_d = d + 0.0045;
        let bevel = (-bevel_d * bevel_d * 20000.0).exp();
        let crest_d = d + 0.0065;
        let crest = (-crest_d * crest_d * 70000.0).exp();
        let fold_d = d - 0.008;
        let lower_fold = (-fold_d * fold_d * 9500.0).exp();

        // A bend facing the shared lamp catches more of its highlight.
        let upper_normal = normalize2([slope, -1.0]);
        let lamp_facing =
            (upper_normal[0] * key_light[0] + upper_normal[1] * key_light[1]).max(0.0);
        let key_glint = 0.52 + 0.48 * lamp_facing.powi(4);
        let travelling = 0.64 + 0.36 * (x * 3.1 - t * (0.9 + fi * 0.25) + fi).sin();

        let depth = 1.0 - fi * 0.18;
        let haze_tint = mix3(sky.accent[0], sky.accent[2], 0.46);
        let body_tint = mix3(sky.accent[0], sky.accent[2], 0.28 + fi * 0.04);

        color = add3(color, scale3(haze_tint, skirt * 0.020 * depth));
        color = add3(color, scale3(body_tint, body * 0.040 * depth));
        color = add3(color, scale3(sky.accent[2], lower_fold * 0.010 * depth));
        color = add3(
            color,
            scale3(
                sky.accent[1],
                (bevel * 0.022 * (0.82 + 0.18 * travelling)
                    + crest * 0.010 * travelling * key_glint)
                    * depth,
            ),
        );
    }

    // Two aurora veils sweep through different thirds of the display.
    let upper_center =
        0.28 + (u * 2.6 + t * 0.16).sin() * 0.10 + (u * 5.4 - t * 0.11).sin() * 0.040;
    let upper_d = v - upper_center;
    let upper_veil = (-upper_d * upper_d * 32.0).exp();
    let upper_crest = (-upper_d * upper_d * 230.0).exp();
    let upper_sheen = 0.72 + 0.28 * (u * 4.2 - t * 0.22).sin();

    let lower_center =
        0.72 + (u * 2.1 - t * 0.13 + 2.4).sin() * 0.12 + (u * 4.7 + t * 0.083).sin() * 0.035;
    let lower_d = v - lower_center;
    let lower_veil = (-lower_d * lower_d * 26.0).exp();
    let lower_crest = (-lower_d * lower_d * 180.0).exp();
    let lower_sheen = 0.74 + 0.26 * (u * 3.7 + t * 0.18 + 1.7).sin();

    color = add3(
        color,
        scale3(
            sky.accent[0],
            (upper_veil * 0.052 + upper_crest * 0.025) * upper_sheen,
        ),
    );
    color = add3(
        color,
        add3(
            scale3(sky.accent[2], lower_veil * 0.16 * lower_sheen),
            scale3(sky.accent[0], lower_crest * 0.028 * lower_sheen),
        ),
    );

    // Vignette, so the edges do not compete with the content.
    let edge = distance([u, v], [0.5, 0.5]);
    scale3(color, 1.0 - smoothstep(0.55, 1.05, edge) * 0.55)
}

/// A huge, feathered pool of light.
fn ambient_field(p: [f32; 2], center: [f32; 2], radius: [f32; 2]) -> f32 {
    let q = [
        (p[0] - center[0]) / radius[0],
        (p[1] - center[1]) / radius[1],
    ];
    (-(q[0] * q[0] + q[1] * q[1]) * 1.65).exp()
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn distance(a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (a[0] - b[0], a[1] - b[1]);
    (dx * dx + dy * dy).sqrt()
}

fn normalize2(v: [f32; 2]) -> [f32; 2] {
    let length = (v[0] * v[0] + v[1] * v[1]).sqrt();
    if length == 0.0 {
        [0.0, 0.0]
    } else {
        [v[0] / length, v[1] / length]
    }
}

fn mix3(a: [f32; 3], b: [f32; 3], amount: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * amount,
        a[1] + (b[1] - a[1]) * amount,
        a[2] + (b[2] - a[2]) * amount,
    ]
}

fn add3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale3(a: [f32; 3], amount: f32) -> [f32; 3] {
    [a[0] * amount, a[1] * amount, a[2] * amount]
}

/// Draw the wallpaper into an 8-bit `RGBA` image, sRGB-encoded and opaque.
///
/// The result is meant to be scaled up to the display: it is a bridge shown
/// for a fraction of a second, and every term in [`sample`] is broad enough
/// that bilinear filtering from a few hundred rows is not the difference
/// anybody is looking at. Rendering it at the real panel size would cost more
/// milliseconds than the black screen it exists to remove.
///
/// Drawn on every core the machine has, because this runs at the one moment
/// nothing else on it is running and everything waiting behind it is what the
/// user is waiting for. [`sample`] is thirty-odd transcendental functions per
/// pixel — a third of a microsecond each — so a 640×360 bridge frame is the
/// best part of a tenth of a second on one core, spent between taking the
/// displays and starting the session shell. Split across a desktop's cores it
/// is a handful of milliseconds, and the arithmetic is untouched: every band
/// is a range of rows, and a row does not depend on the row above it.
pub fn image(sky: &Sky, width: u32, height: u32, aspect: f32, t: f32) -> Vec<u8> {
    let mut pixels = vec![0_u8; width as usize * height as usize * 4];
    let stride = width as usize * 4;
    // One band per core, and never fewer rows in a band than there is thread
    // to start it: a tiny image — the tests draw 16×9 ones — is drawn where it
    // stands rather than handed round.
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    let bands = cores.max(1).min((height as usize / 16).max(1));
    if bands == 1 {
        draw_band(sky, &mut pixels, 0, width, height, aspect, t);
        return pixels;
    }
    let rows = height as usize / bands + usize::from(height as usize % bands != 0);
    // `scope` rather than `spawn`, so nothing outlives this call and a thread
    // that cannot be started is not a wallpaper that never arrives: the scope
    // itself panics only if the closure does, and this one cannot.
    std::thread::scope(|scope| {
        for (band, slice) in pixels.chunks_mut(rows * stride).enumerate() {
            let first = (band * rows) as u32;
            scope.spawn(move || draw_band(sky, slice, first, width, height, aspect, t));
        }
    });
    pixels
}

/// Draw the rows of `pixels` that start at `first`, into a slice holding only
/// those rows.
///
/// `height` is the whole image's, not the band's: `v` has to run 0..1 over the
/// picture rather than over the piece of it being drawn.
fn draw_band(
    sky: &Sky,
    pixels: &mut [u8],
    first: u32,
    width: u32,
    height: u32,
    aspect: f32,
    t: f32,
) {
    for (row, line) in pixels.chunks_mut(width as usize * 4).enumerate() {
        // Sample pixel centres: the vignette and the gradient both reach the
        // very edge, and sampling corners would shift the whole image by half
        // a low-resolution pixel once it is scaled up.
        let v = (first as f32 + row as f32 + 0.5) / height as f32;
        for (column, pixel) in line.chunks_exact_mut(4).enumerate() {
            let u = (column as f32 + 0.5) / width as f32;
            let color = sample(sky, [u, v], aspect, t);
            for channel in 0..3 {
                pixel[channel] =
                    (linear_to_srgb(color[channel].clamp(0.0, 1.0)) * 255.0).round() as u8;
            }
            pixel[3] = 0xff;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_palette_has_a_distinct_name_and_sky() {
        for (index, palette) in PALETTES.iter().enumerate() {
            for other in &PALETTES[index + 1..] {
                assert_ne!(palette.name, other.name);
                assert_ne!(palette.sky, other.sky);
            }
        }
    }

    #[test]
    fn an_unknown_accent_falls_back_to_the_first() {
        assert_eq!(palette("Green").name, "Green");
        assert_eq!(palette("green").name, PALETTES[0].name);
        assert_eq!(palette("").name, PALETTES[0].name);
    }

    #[test]
    fn the_transfer_function_round_trips() {
        for step in 0..=32 {
            let value = step as f32 / 32.0;
            let round_tripped = linear_to_srgb(srgb_to_linear(value));
            assert!((round_tripped - value).abs() < 1e-4, "{value}");
        }
    }

    #[test]
    fn the_wallpaper_is_dark_opaque_and_within_range_everywhere() {
        let sky = Sky::new(palette("Purple"));
        for &t in &[0.0_f32, 1.5, 97.0, 3600.0] {
            for &aspect in &[16.0 / 9.0_f32, 4.0 / 3.0, 32.0 / 9.0] {
                for step in 0..=8 {
                    let position = step as f32 / 8.0;
                    for uv in [[position, 0.5_f32], [0.5, position]] {
                        let color = sample(&sky, uv, aspect, t);
                        for channel in color {
                            assert!(channel.is_finite(), "{channel} at {uv:?}");
                            assert!((0.0..=1.0).contains(&channel), "{channel} at {uv:?}");
                        }
                    }
                }
            }
        }
    }

    /// Drawing on twelve cores has to give the picture drawing on one gives.
    ///
    /// The seam between two bands is where a mistake would show — a row taken
    /// from the band's own top rather than the picture's — so this is checked
    /// at every size the banding divides differently, including the ones where
    /// the last band is short.
    #[test]
    fn every_band_draws_the_rows_it_was_given() {
        let sky = Sky::new(palette("Blue"));
        for height in [1_u32, 15, 16, 17, 64, 90, 101] {
            let width = 32;
            let whole = image(&sky, width, height, 16.0 / 9.0, 7.0);
            assert_eq!(whole.len(), width as usize * height as usize * 4);
            let mut one_core = vec![0_u8; whole.len()];
            draw_band(&sky, &mut one_core, 0, width, height, 16.0 / 9.0, 7.0);
            assert_eq!(whole, one_core, "{height} rows");
        }
    }

    #[test]
    fn the_image_is_opaque_and_the_expected_size() {
        let sky = Sky::new(palette("Blue"));
        let pixels = image(&sky, 16, 9, 16.0 / 9.0, 12.0);
        assert_eq!(pixels.len(), 16 * 9 * 4);
        assert!(pixels.chunks_exact(4).all(|pixel| pixel[3] == 0xff));
        // Dark, but not the black screen this exists to replace.
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[..3] != [0, 0, 0]));
    }

    /// The clock the display manager hands over is what makes the compositor's
    /// bridge and the shell's first frame the same moment. A wallpaper that
    /// ignored `t` would still be a picture, but it would be the wrong one.
    #[test]
    fn the_wallpaper_moves_with_its_clock() {
        let sky = Sky::new(palette("Purple"));
        let at = |t| sample(&sky, [0.5, 0.6], 16.0 / 9.0, t);
        assert_ne!(at(0.0), at(4.0));
    }
}
