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

/// How much material one half of the shell is drawn with.
///
/// The shell's own look is expensive on purpose: the current is three sheets of
/// water lit as bodies, and every glyph is a bead of water shaded out of its own
/// distance field. On a machine that cannot afford that, the whole of it can be
/// stood down to the drawing underneath — and the *drawing* is the same either
/// way, which is what keeps the shell recognisable rather than reduced.
///
/// One value of this is *half* an answer. The wallpaper and the marks are two
/// separate settings — Settings > Appearance > Theme > Wallpaper and > Icons,
/// written to `shell.toml` as `theme-wallpaper` and `theme-icons` — because the
/// two cost their own money and are wanted in their own combinations: the water
/// is a full-screen evaluation every frame and a mark is a few dozen pixels, so
/// a machine that cannot afford the first may very well afford the second, and
/// somebody who simply prefers flat marks over a moving wallpaper is entitled to
/// that. This enum is the answer either of them takes.
///
/// The display manager reads both keys, so a login screen never arrives in a
/// material the session is not using; the compositor reads only the wallpaper's,
/// because a bridge frame is a wallpaper and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Style {
    /// Water: the band of three sheets, and beaded glyphs.
    #[default]
    Default,
    /// The plainer material a slow machine asks for: the current as three fine
    /// glass-silk lines, which is what this shell drew before the band, and
    /// glyphs as their own flat shapes.
    Simple,
    /// Not a material at all: a picture or a film of the user's own, standing
    /// where the scene would be drawn.
    ///
    /// The wallpaper's value only — see [`WALLPAPER_STYLES`]. There is nothing
    /// for it to mean about a mark, and the one place it is offered is the one
    /// place there is a file to answer with.
    ///
    /// Every reader of this setting but the shell draws [`Style::analytic`]
    /// instead, and none of them is being short-changed: the file is under the
    /// user's home, and the two readers are a compositor bridging the start of a
    /// session and a login screen standing in front of every account on the
    /// machine. Neither is in a position to open it.
    Custom,
}

/// Every style the shell offers *either* half of itself, in the order Settings
/// lists them. The first is what an unreadable or unrecognised setting falls
/// back to.
pub const STYLES: [&str; 2] = ["Default", "Simple"];

/// What the wallpaper may be set to, which is those two and the user's own
/// picture.
///
/// A separate list rather than a third entry in [`STYLES`], because the Theme
/// page asks one question about two things and only one of them has an answer
/// of this kind. "Custom wallpaper" under Icons would be a row offering to draw
/// every mark in the shell out of somebody's holiday photograph.
pub const WALLPAPER_STYLES: [&str; 3] = ["Default", "Simple", CUSTOM];

/// The name the user's own picture is written down and listed under.
///
/// Spelled out in full rather than as `Custom`, because the row it names stands
/// in a column with Default and Simple in it and has to say what it is a custom
/// *of*.
pub const CUSTOM: &str = "Custom wallpaper";

/// The style of that name, matched exactly, case-sensitively, the way
/// [`palette`] matches an accent: the three names are the whole of either
/// setting's domain and quietly accepting `simple` would make two spellings of
/// one value.
pub fn style(name: &str) -> Style {
    match name {
        "Simple" => Style::Simple,
        CUSTOM => Style::Custom,
        _ => Style::Default,
    }
}

impl Style {
    /// The name this style is written down as, which is also what Settings
    /// shows.
    pub fn name(self) -> &'static str {
        match self {
            Self::Default => STYLES[0],
            Self::Simple => STYLES[1],
            Self::Custom => CUSTOM,
        }
    }

    /// The material to *draw* the scene in, for anything that draws the scene.
    ///
    /// [`Style::Custom`] is an answer that replaces the scene with a file rather
    /// than one that changes what the scene is made of, so every reader that has
    /// only the scene — this crate's own [`sample`], the compositor's bridge
    /// frame, the login screen in front of an account it cannot read — asks for
    /// this first and gets the shell's own material back. One place says so, so
    /// that no reader has to decide it again.
    pub fn analytic(self) -> Self {
        match self {
            Self::Custom => Self::Default,
            other => other,
        }
    }
}

/// What this wallpaper *is*, for anything that hands its clock to anything
/// else.
///
/// A scene time only means something to a process that draws the same scene, so
/// every handoff record carries this string and a reader that does not
/// recognise it starts the animation again rather than continuing from a phase
/// of a picture it is not drawing. Bump it whenever a change here or in
/// `lxb-desktop`'s `shaders.wgsl` would draw a different frame at the same
/// time — and bump the display manager's copy with it, in the same breath:
/// CEDM vendors this scene rather than depending on this crate, and a version
/// only does its job when both ends of the handover agree what it names.
pub const VISUAL: &str = "lxb-wallpaper-v2";

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
    Palette {
        name: "Teal",
        sky: [
            Color(0x08302B),
            Color(0x020F0D),
            Color(0x0B4038),
            Color(0x041614),
        ],
        accent: [Color(0x14B8A6), Color(0x5EEAD4), Color(0x134E4A)],
        glow: Color(0x115E59),
    },
    Palette {
        name: "Indigo",
        sky: [
            Color(0x101128),
            Color(0x030409),
            Color(0x15163A),
            Color(0x070813),
        ],
        accent: [Color(0xA5B4FC), Color(0xC7D2FE), Color(0x312E81)],
        glow: Color(0x252270),
    },
    Palette {
        name: "Pink",
        sky: [
            Color(0x2C0A1E),
            Color(0x0C0207),
            Color(0x3E0D28),
            Color(0x160410),
        ],
        accent: [Color(0xEC4899), Color(0xF9A8D4), Color(0x831843)],
        glow: Color(0x861042),
    },
    Palette {
        name: "Orange",
        sky: [
            Color(0x301206),
            Color(0x0D0402),
            Color(0x421A07),
            Color(0x160702),
        ],
        accent: [Color(0xF97316), Color(0xFDBA74), Color(0x7C2D12)],
        glow: Color(0x842C0F),
    },
    Palette {
        name: "White",
        sky: [
            Color(0x151515),
            Color(0x040404),
            Color(0x1E1E1E),
            Color(0x090909),
        ],
        accent: [Color(0xE4E4E4), Color(0xF5F5F5), Color(0x3D3D3D)],
        glow: Color(0x444444),
    },
    Palette {
        name: "Silver",
        sky: [
            Color(0x2A2A2A),
            Color(0x0B0B0B),
            Color(0x383838),
            Color(0x131313),
        ],
        accent: [Color(0xA6A6A6), Color(0xD6D6D6), Color(0x4A4A4A)],
        glow: Color(0x6B6B6B),
    },
    Palette {
        name: "Black",
        sky: [
            Color(0x141414),
            Color(0x030303),
            Color(0x1C1C1C),
            Color(0x070707),
        ],
        accent: [Color(0x525252), Color(0x8A8A8A), Color(0x1F1F1F)],
        glow: Color(0x2E2E2E),
    },
];

/// The palette saved under `name`, or the default when it is not one of them.
///
/// Exact, case-sensitive matching, the same rule the shell and the display
/// manager both apply: the names above are the whole of the setting's domain,
/// and quietly accepting `blue` would make two spellings of one value.
pub fn palette(name: &str) -> &'static Palette {
    PALETTES
        .iter()
        .find(|palette| palette.name == name)
        .unwrap_or(&PALETTES[0])
}

/// One accent's colours resolved into linear light, and the material the scene
/// is drawn in: everything that is constant across one image rather than worked
/// out once per pixel.
#[derive(Debug, Clone, Copy)]
pub struct Sky {
    sky: [[f32; 3]; 4],
    accent: [[f32; 3]; 3],
    glow: [f32; 3],
    style: Style,
}

impl Sky {
    /// The shell's own material, which is what a caller with no setting to
    /// consult should draw.
    pub fn new(palette: &Palette) -> Self {
        Self::styled(palette, Style::Default)
    }

    /// The same, in the style the user chose.
    pub fn styled(palette: &Palette, style: Style) -> Self {
        Self {
            style,
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
/// `footprint` is how much of that 0..1 one sample stands for — `[1 / width,
/// 1 / height]` of the picture being drawn. Everything here is a smooth field
/// except the band of water, which has silhouettes, and a silhouette has to
/// know how big a sample is or it lands as a staircase. Nothing else reads it.
///
/// This is `wallpaper()` from `shaders.wgsl` with `soften` and `lod` at zero
/// and no key art: the softened variants are for the guide's backdrop and the
/// glass panes, and there is no artwork to sample before the shell has read
/// the user's library. Keep the arithmetic in step with that function, term
/// for term, and bump the visual identity in the handoff record when it
/// changes in a way that would draw a different frame at the same `t`.
///
/// `footprint` is the one thing here that is not part of that identity: it
/// says how finely this picture is being read, not what the picture *is*, and
/// two readers drawing the same scene at two sizes are still on the same
/// clock. Bumping the identity over it would restart the animation at a
/// handover to avoid a difference of half a pixel of edge.
pub fn sample(sky: &Sky, uv: [f32; 2], aspect: f32, t: f32, footprint: [f32; 2]) -> [f32; 3] {
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

    // The current: the moving thing in the middle of the picture, in
    // whichever material this shell is set to draw it in.
    color = match sky.style.analytic() {
        Style::Simple => silk(sky, color, [u, v], aspect, t),
        // Default, and the user's own picture, which nothing that draws this
        // function has open. See [`Style::analytic`].
        _ => water(sky, color, [u, v], aspect, t, footprint),
    };

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

/// How wide a sheet still is when the display sees it exactly edge-on, as a
/// share of its own width.
///
/// Rounded off just short of nothing: a sheet with no width at all is a crease
/// rather than a fold, and its lighting degenerates on the singularity.
const BAND_FOLD: f32 = 0.020;

/// How much the face of a sheet bows between its two lips, as a slope at the
/// edge of the flat part.
///
/// Water has no flat faces. A face that really is flat carries one surface
/// angle across its whole width, catches the sharp light everywhere at once,
/// and leaves a straight-sided plateau on the ribbon.
const BAND_BOW: f32 = 0.50;

/// The most of their two half-widths that can stand between two neighbouring
/// ribbons of the current.
///
/// The whole of what the band promises: **under one**, so two sheets always
/// overlap, whatever the twist has done to either of their widths. The gap
/// between them is not a distance, it is a share of a width, and it shrinks
/// when they do. Left far enough under one to cover the feathering of both
/// silhouettes, so what the *eye* sees is joined and not only the geometry —
/// `the_three_ribbons_of_the_current_are_never_apart` holds it to that.
const BAND_SHARE: f32 = 0.88;

/// How far either side of a sheet's edge its silhouette is spread, in samples
/// of the picture being drawn.
///
/// A box filter a sample wide would spread it half a sample each way. This is
/// wider because the fade is a smoothstep rather than a ramp, and a
/// smoothstep does most of its travelling in the middle of the interval it is
/// given: measured against a picture drawn with sixty-four samples to the
/// pixel, the error stops falling at about this much and blurring it further
/// only costs sharpness.
const BAND_EDGE_SAMPLES: f32 = 0.8;

/// How much of a sheet stands over a sample `reach` of the way across it.
///
/// Feathered only in its last few percent: water holds its own edge, and the
/// rim light needs a surface to sit on. That is the fade as authored, and what
/// the shell's own frame very nearly draws.
///
/// Widened by a sample either side of the edge, which is what makes the same
/// band bear being drawn small — into the compositor's bridge frame, a fifth of
/// a display across, or into a card's miniature. This is the whole of the
/// anti-aliasing the band gets and all it needs: the silhouette is the only
/// place this picture stops being smooth, and everything sharp about a sheet —
/// the lip, the glint, the dispersion — is carried by this number and goes soft
/// with it.
///
/// Either side of the edge, rather than inwards from it: a fade that only ate
/// into the sheet would thin the water as the picture got smaller, and where a
/// ribbon is pinched nearly edge-on it would take most of it. Spread
/// symmetrically, this is close to what a sample-wide box filter of the same
/// edge lands on, which is the picture more samples would converge to.
fn silhouette(reach: f32, width: f32, sample: f32) -> f32 {
    let spread = BAND_EDGE_SAMPLES * sample / width;
    1.0 - smoothstep(0.94 - spread, 1.0 + spread, reach)
}

/// The twist of each ribbon of the current, how wide that leaves it, and where
/// it stands across the band, at one place and time.
///
/// `gather` is how much of the band's spread survives here, which is what
/// closes the three together at both ends of the display.
///
/// The widths come first because the offsets are made out of them: the middle
/// ribbon *is* the band, and the outer two are pushed off it by a share of the
/// two half-widths that meet there — never as much as [`BAND_SHARE`] of it, and
/// through zero, so they cross without ever coming apart.
fn stack(u: f32, t: f32, gather: f32) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let mut tilt = [0.0_f32; 3];
    let mut width = [0.0_f32; 3];
    for i in 0..3 {
        let fi = i as f32;
        let x = u * (2.0 + fi * 0.6);
        // How far this sheet has turned about its own travel. Three opposing
        // clocks whose periods do not divide one another, so the twist never
        // settles into a pattern that repeats down the ribbon, and deep enough
        // that it carries the sheet past edge-on — which is what a wrapping
        // ribbon does, and where it pinches.
        let turning = (x * 2.10 - t * (0.23 + fi * 0.05) + fi * 1.9).sin() * 0.95
            + (x * 1.25 + t * 0.15 + fi * 2.7).sin() * 0.55
            + (x * 4.30 - t * 0.35 + fi * 0.7).sin() * 0.22;
        // Squared, keeping its sign: a ribbon lies open for a long run and then
        // turns through its twist quickly, rather than rolling evenly the whole
        // way along like a screw.
        tilt[i] = turning * turning.abs() * 0.62 + (x * 7.0 + t * 0.9 + fi).sin() * 0.08;
        let broad = (tilt[i].cos() * tilt[i].cos() + BAND_FOLD).sqrt();
        width[i] = (0.0640 - 0.0110 * fi) * broad;
    }

    let lift = BAND_SHARE * (0.32 + 0.68 * (u * 4.3 - t * 0.37).sin());
    let drop = BAND_SHARE * (0.32 + 0.68 * (u * 3.1 + t * 0.29 + 2.2).sin());
    let offset = [
        -(width[0] + width[1]) * lift * gather,
        0.0,
        (width[1] + width[2]) * drop * gather,
    ];
    (tilt, width, offset)
}

/// The current as the shell's own material: one band of water, three ribbons
/// thick, drifting through a broad lane below the cross point.
///
/// Takes the scene as it stands and hands it back with the band drawn into it,
/// because water is a body rather than a glow — it takes light out of what
/// stands behind it before putting any of its own back.
fn water(
    sky: &Sky,
    into: [f32; 3],
    point: [f32; 2],
    aspect: f32,
    t: f32,
    footprint: [f32; 2],
) -> [f32; 3] {
    let (u, v) = (point[0], point[1]);
    let mut color = into;
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
    // The three are stacked along one spine, and they cannot come apart.
    // Each outer ribbon is pushed off the middle one by a *share* of the two
    // half-widths that meet there rather than by a length of its own: a share
    // that passes through zero, so they cross, and never reaches one, so the
    // sheets always overlap. Nothing the twist or the drift does can open a
    // gap between them, because the space between them is not a distance — it
    // is a fraction of a width that shrinks when they do.
    //
    // Nothing is refracted from behind: a sheet this thin in front of a field
    // this smooth displaces nothing the eye could see, and what says water
    // here is the geometry of the surface.
    //
    // How much of a half-width the rolled lip takes. The rest is flat face,
    // and a ribbon narrower than twice this is all lip — which is correct, and
    // is what keeps the finer strands beads of water rather than flat rails.
    let lip = 0.45;
    // The lamp the whole shell shares, and the half vector between it and an
    // eye looking straight into the display.
    let key = normalize3([-0.42, -0.66, 0.62]);
    let half_vector = normalize3([key[0], key[1], key[2] + 1.0]);

    // Everything the band is made of is gathered in towards its lane at both
    // ends of the display: the ribbons come off the left edge close together,
    // open apart across the middle, and close again on the way off the right.
    // Never all the way to nothing, or the band would leave as one line.
    let gather = 0.28 + 0.72 * (std::f32::consts::PI * u).sin();
    let gather_slope = 0.72 * std::f32::consts::PI * (std::f32::consts::PI * u).cos();

    // The spine: the one curve the band is stacked along, and so the only one
    // whose slope has to be measured.
    let spine_a = u * 6.8 + t * 0.56;
    let spine_b = u * 3.4 - t * 0.39 + 0.8;
    let swing = spine_a.sin() * 0.055 + spine_b.sin() * 0.085;
    let swing_slope = spine_a.cos() * 0.055 * 6.8 + spine_b.cos() * 0.085 * 3.4;
    let spine = 0.62 + swing * gather;
    // Measured across the curve rather than vertically, so a steep section
    // does not grow visibly thicker than a flat one. In the same
    // physical-screen units as y, so wide outputs do not over-correct either
    // the width or the light angle. The gathering is part of the curve, and so
    // is its slope.
    let slope = (swing_slope * gather + swing * gather_slope) / aspect;
    let across = normalize2([slope, -1.0]);
    // And along the band, in those same units.
    let along = [-across[1], across[0]];
    let band = (v - spine) / (1.0 + slope * slope).sqrt();
    // How far across the band one sample reaches, in those same units: the
    // sample's own two sides, each as much of them as points across the curve.
    // Nothing else in this picture needs it — every other term here is a field
    // that changes slowly enough to be read one point at a time — but the
    // sheets have edges, and an edge drawn from a single point either lands in
    // a sample or does not.
    let sample = (across[0] * footprint[0] * aspect).abs() + (across[1] * footprint[1]).abs();

    // Every ribbon's twist and width, and where each of them stands across the
    // band, before any of them is drawn.
    let (tilt, width, offset) = stack(u, t, gather);

    for i in 0..3 {
        let fi = i as f32;
        let x = u * (2.0 + fi * 0.6);
        let d = band - offset[i];
        let width = width[i];

        // The small water on top of the twist, as the slope of waves
        // travelling along the ribbon's own length.
        let along_wave = (x * 9.0 - t * 1.1 + fi * 2.0).cos() * 0.34
            + (x * 23.0 + t * 1.9 + fi * 1.3).cos() * 0.14;

        let reach = (d / width).abs();
        // Where across the ribbon this pixel is, signed, for the light that
        // travels through the sheet at an angle.
        let s_across = (d / width).clamp(-1.0, 1.0);
        let cover = silhouette(reach, width, sample);
        // Where in the rolled-over lip this pixel is: 0 at the outer edge, 1
        // where the flat face begins.
        let inset = ((1.0 - reach) / lip).clamp(0.0, 1.0);
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
        let wall = normalize2([-d.signum() * bevel_slope(inset) - s_across * BAND_BOW, 1.0]);
        let (turn, level) = (tilt[i].sin(), tilt[i].cos());
        // How much of this sheet's own width the display sees, which `stack`
        // has already turned into how wide it is drawn. The same water behind
        // less display is brighter for it, up to a ceiling — or the pinch
        // itself would be the brightest thing on screen.
        let broad = (level * level + BAND_FOLD).sqrt();
        let fold = (1.0 / broad).min(2.2);
        let face = [
            wall[0] * level + wall[1] * turn,
            wall[1] * level - wall[0] * turn,
        ];
        // The surface itself: that angle across the ribbon, the wave slope
        // along it, and what is left of it facing the display.
        let surface = normalize3([
            across[0] * face[0] + along[0] * along_wave * face[1],
            across[1] * face[0] + along[1] * along_wave * face[1],
            face[1],
        ]);

        let facing = dot3(surface, key).clamp(0.0, 1.0);
        let fresnel = 0.04 + 0.96 * (1.0 - surface[2].clamp(0.0, 1.0)).powi(5);
        let glint = dot3(surface, half_vector).max(0.0).powi(42);
        // The room the sheet hands back at a grazing angle. Only the
        // brightness of it is kept, the way a glyph keeps only the brightness
        // of the sky it reflects: the colour belongs to the accent.
        let mirrored = reflect3([0.0, 0.0, -1.0], surface);
        let room = 0.42 + 0.73 * (0.5 - 0.5 * dot3(mirrored, key));
        // Only the sharp light travels quickly, so the sheet glistens without
        // the whole ribbon pulsing.
        let travelling = 0.60 + 0.40 * (x * 3.1 - t * (0.9 + fi * 0.25) + fi).sin();
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
        let focusing = 0.5 + 0.5 * (x * 2.3 - t * 0.5 + fi).sin();
        let gathered = (x * 23.0 + t * 1.9 + fi * 1.3 + s_across * 3.4)
            .sin()
            .max(0.0)
            .powi(5)
            * (0.15 + 0.85 * focusing * focusing);

        let depth = 1.0 - fi * 0.26;
        let skirt = (-d * d * 90.0).exp();
        // The shadow the sheet throws on the wallpaper, down-light of itself.
        // The one thing that says the band is in front of the scene rather
        // than mixed into it.
        let shadow_d = d - width - 0.010;
        let shadow = (-shadow_d * shadow_d * 1400.0).exp() * (1.0 - cover);
        // And the line its curve gathers on the far side of that shadow, where
        // the light it let through comes back together.
        let caustic_d = d - width - 0.0040;
        let caustic = (-caustic_d * caustic_d * 30000.0).exp() * broad;

        let haze_tint = mix3(sky.accent[0], sky.accent[2], 0.46);
        let body_tint = mix3(sky.accent[0], sky.accent[2], 0.24 + fi * 0.05 + 0.36 * rise);
        // A thick edge splits what passes through it into colour: warm above
        // the lip, cold below it. Small, because it is a cue and not a prism.
        let split = 0.055 * (1.0 - inset) * (1.0 - inset) * cover * depth * -d.signum();

        let water = cover * fold * depth;
        // Water is a body: it takes light out of what stands behind it and
        // shades what stands beside it, before any of its own is added.
        color = scale3(color, 1.0 - cover * (0.05 + 0.11 * rise) * depth);
        color = scale3(color, 1.0 - shadow * 0.20 * depth);
        color = add3(color, scale3(haze_tint, skirt * 0.007 * depth));
        color = add3(
            color,
            scale3(body_tint, water * (0.005 + 0.012 * rise + 0.028 * facing)),
        );
        color = add3(color, scale3(sky.accent[1], fresnel * room * water * 0.100));
        color = add3(
            color,
            scale3(sky.accent[1], glint * water * 0.120 * travelling),
        );
        color = add3(
            color,
            scale3(sky.accent[1], gathered * cover * rise * 0.011 * depth),
        );
        color = add3(color, scale3(sky.accent[1], caustic * 0.011 * depth));
        color = [color[0] * (1.0 + split), color[1], color[2] * (1.0 - split)];
    }
    color
}

/// The current as the plainer material: three fine glass-silk ribbons moving
/// together through the same lane.
///
/// What this shell drew before the band, kept for the Simple theme rather than
/// deleted — a machine that cannot afford the water still gets a current, and
/// this is the one that was tuned for a year to sit under the bar without
/// competing with it. Each ribbon is a broad skirt, a steady translucent body,
/// a deep lower fold and an accent-soft bevel catching the shell's upper-left
/// lamp; only the glossy layers carry the quicker travelling sheen, so the
/// whole line does not pulse like neon.
///
/// Adds and never subtracts, which is the other half of why it is cheap: no
/// pixel behind it has to be read back and dimmed.
fn silk(sky: &Sky, into: [f32; 3], point: [f32; 2], aspect: f32, t: f32) -> [f32; 3] {
    let (u, v) = (point[0], point[1]);
    let mut color = into;
    // The current: three fine glass-silk ribbons moving together through
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
    color
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

/// How steep the rolled lip is `inset` of the way in: 0 at the outer edge, 1
/// where the flat face begins. A quarter-round profile, and the same one
/// `bevel_rise` in `shaders.wgsl` gives every other piece of water in the
/// shell. `rise` is how much of the full depth has been reached there.
fn bevel_rise(inset: f32) -> f32 {
    (1.0 - (1.0 - inset) * (1.0 - inset)).max(0.0).sqrt()
}

/// Held short of vertical at the very edge, for the reason `bevel_slope` in
/// `shaders.wgsl` holds it there: a truly vertical face is edge-on to an eye
/// looking straight into it, and the lighting degenerates on the singularity
/// rather than on the last pixel of a steep curve.
fn bevel_slope(inset: f32) -> f32 {
    (1.0 - inset) / bevel_rise(inset).max(0.16)
}

fn reflect3(v: [f32; 3], normal: [f32; 3]) -> [f32; 3] {
    let twice = 2.0 * dot3(v, normal);
    [
        v[0] - twice * normal[0],
        v[1] - twice * normal[1],
        v[2] - twice * normal[2],
    ]
}

fn normalize3(v: [f32; 3]) -> [f32; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if length == 0.0 {
        [0.0, 0.0, 0.0]
    } else {
        [v[0] / length, v[1] / length, v[2] / length]
    }
}

fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
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
            let color = sample(
                sky,
                [u, v],
                aspect,
                t,
                [1.0 / width as f32, 1.0 / height as f32],
            );
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

    /// One pixel of a display, for the tests that need to ask for a sample
    /// rather than to say anything about the size of one.
    const PIXEL: [f32; 2] = [1.0 / 1920.0, 1.0 / 1080.0];

    /// The two sizes this wallpaper is really drawn at: the shell's own frame,
    /// and the bridge frame the compositor paints small and scales up.
    const DRAWN_AT: [(&str, [f32; 2]); 2] = [
        ("the shell's frame", PIXEL),
        ("a bridge frame", [1.0 / 640.0, 1.0 / 360.0]),
    ];

    /// The one thing the current promises about itself: it is a band, not three
    /// lines that happen to be near each other. Two neighbouring ribbons always
    /// overlap by enough that the feathering of both silhouettes is inside the
    /// overlap, so there is nowhere on the display, at any time, where the eye
    /// could find the wallpaper between them.
    ///
    /// This is the geometry alone, and the fixed part of the feathering with
    /// it. The rest of the feathering is as wide as a sample, which the
    /// geometry knows nothing about;
    /// `the_band_is_still_joined_at_the_size_it_is_drawn` takes the promise the
    /// rest of the way at both sizes anything draws this at.
    ///
    /// Swept rather than reasoned about, because the widths come out of a twist
    /// of three sines squared and the offsets come out of the widths: the shape
    /// of that is not something to hold in the head and be sure of.
    #[test]
    fn the_three_ribbons_of_the_current_are_never_apart() {
        // Where the silhouette starts to fade, out of `sample`. The overlap has
        // to be wider than the two fading edges put together.
        const FEATHER: f32 = 0.06;
        for step in 0..1200 {
            // Coarser in time than across the display, and prime-ish steps, so
            // the sweep does not keep landing on the same phase of the clocks.
            let t = step as f32 * 0.37;
            for column in 0..=240 {
                let u = column as f32 / 240.0;
                let gather = 0.28 + 0.72 * (std::f32::consts::PI * u).sin();
                let (_, width, offset) = stack(u, t, gather);
                for pair in 0..2 {
                    let above = offset[pair] + width[pair];
                    let below = offset[pair + 1] - width[pair + 1];
                    let seen = FEATHER * (width[pair] + width[pair + 1]);
                    assert!(
                        above - below >= seen,
                        "ribbons {pair} and {} part at u={u} t={t}: \
                         {above} to {below}, needing {seen} of overlap",
                        pair + 1,
                    );
                }
            }
        }
    }

    /// And the same promise once the silhouettes are feathered to the size of
    /// the sample they are drawn with, which is the part the geometry cannot
    /// see: a bridge frame is a fifth of the display across, so a sample there
    /// is worth three of the shell's, and where a sheet is pinched nearly
    /// edge-on that is most of its width.
    ///
    /// Walked across every seam rather than argued about, and stated as what
    /// the eye would actually find: how much of the wallpaper is still showing
    /// at the least covered point between two ribbons, with both of their
    /// feathered silhouettes over it.
    #[test]
    fn the_band_is_still_joined_at_the_size_it_is_drawn() {
        for (label, footprint) in DRAWN_AT {
            let mut least = 1.0_f32;
            for step in 0..600 {
                // Coarser in time than across the display, and prime-ish steps,
                // so the sweep does not keep landing on the same phase of the
                // clocks.
                let t = step as f32 * 0.37;
                for column in 0..=240 {
                    let u = column as f32 / 240.0;
                    let gather = 0.28 + 0.72 * (std::f32::consts::PI * u).sin();
                    let (_, width, offset) = stack(u, t, gather);
                    // A sample of that picture, across a band lying flat. The
                    // spine's own slope only ever shortens this, and a seam is
                    // measured across the band in any case.
                    let sample = footprint[1];
                    let cover = |ribbon: usize, at: f32| {
                        let reach = ((at - offset[ribbon]) / width[ribbon]).abs();
                        silhouette(reach, width[ribbon], sample)
                    };
                    for pair in 0..2 {
                        for slice in 0..=64 {
                            let across = slice as f32 / 64.0;
                            let at = offset[pair] + (offset[pair + 1] - offset[pair]) * across;
                            let covered =
                                1.0 - (1.0 - cover(pair, at)) * (1.0 - cover(pair + 1, at));
                            least = least.min(covered);
                        }
                    }
                }
            }
            // A twentieth of the wallpaper showing through the thinnest part of
            // a seam is nothing the eye can find; a seam that had come open
            // would be showing all of it.
            assert!(
                least >= 0.95,
                "{label}: a seam is only {least} covered at its thinnest",
            );
        }
    }

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

    /// The name every reader of the setting has to agree on, and the one thing
    /// this crate promises about it: that a reader with no file to open draws
    /// the shell's own scene rather than nothing.
    #[test]
    fn a_custom_wallpaper_is_named_here_and_drawn_as_the_default_one() {
        assert_eq!(style(CUSTOM), Style::Custom);
        assert_eq!(Style::Custom.name(), CUSTOM);
        assert_eq!(Style::Custom.analytic(), Style::Default);
        assert_eq!(Style::Simple.analytic(), Style::Simple);
        // Offered under the wallpaper and nowhere else: a mark drawn out of
        // somebody's photograph is not a thing this setting can mean.
        assert!(WALLPAPER_STYLES.contains(&CUSTOM));
        assert!(!STYLES.contains(&CUSTOM));
        for name in STYLES {
            assert!(WALLPAPER_STYLES.contains(&name), "{name}");
        }
    }

    /// A bridge frame is drawn by a compositor that cannot read the user's
    /// picture, so the two settings it *can* draw are the whole of what it
    /// distinguishes — and a custom wallpaper has to land on one of them rather
    /// than on a frame of its own.
    #[test]
    fn the_bridge_frame_of_a_custom_wallpaper_is_the_default_one() {
        let palette = palette("Purple");
        let custom = Sky::styled(palette, Style::Custom);
        let default = Sky::styled(palette, Style::Default);
        let simple = Sky::styled(palette, Style::Simple);
        let mut anywhere_the_material_shows = false;
        for row in 0..24 {
            for column in 0..24 {
                let uv = [column as f32 / 23.0, row as f32 / 23.0];
                let at = |sky: &Sky| sample(sky, uv, 16.0 / 9.0, 3.5, PIXEL);
                assert_eq!(at(&custom), at(&default), "{uv:?}");
                anywhere_the_material_shows |= at(&custom) != at(&simple);
            }
        }
        // And the comparison means something: the two materials do part company
        // somewhere on the screen, so the equality above is not two identical
        // pictures agreeing about nothing.
        assert!(anywhere_the_material_shows);
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
                        let color = sample(&sky, uv, aspect, t, PIXEL);
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
        let at = |t| sample(&sky, [0.5, 0.6], 16.0 / 9.0, t, PIXEL);
        assert_ne!(at(0.0), at(4.0));
    }
}
