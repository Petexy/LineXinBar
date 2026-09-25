//! The shell's palette, in one place.
//!
//! Every colour the shell draws comes from here — the wallpaper's own gradient
//! included, which is why the background shader takes its four key colours as
//! uniforms rather than baking them in. Retheming is then a matter of handing
//! out a different [`Theme`], not of editing a shader.
//!
//! Colours are written the way a designer picks them, as sRGB hex, and
//! converted to linear light on the way to the GPU: the surface is an sRGB
//! target, so a value written raw would come out roughly twice as bright as
//! the hex it was copied from.

use lxb_protocol::wallpaper::{self, Style};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// A colour as authored: 0xRRGGBB in sRGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    /// Linear RGB, for a shader or a [`crate::gpu::Quad`].
    pub fn rgb(self) -> [f32; 3] {
        [
            linear((self.0 >> 16 & 0xff) as f32 / 255.0),
            linear((self.0 >> 8 & 0xff) as f32 / 255.0),
            linear((self.0 & 0xff) as f32 / 255.0),
        ]
    }

    /// The same with an alpha, which is what nearly every call site wants.
    pub fn a(self, alpha: f32) -> [f32; 4] {
        let [r, g, b] = self.rgb();
        [r, g, b, alpha]
    }

    /// This authored colour resolved into the linear-light form the renderer
    /// consumes. Keeping the two types separate lets a transition hold values
    /// between two eight-bit authored colours without quantising every frame.
    fn linear(self) -> LinearColor {
        LinearColor(self.rgb())
    }
}

/// A colour ready to draw, in linear light.
///
/// Authored [`Color`] values remain the stable palette targets and the colour
/// swatches in Settings. This is the runtime value between those targets while
/// the accent is changing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LinearColor([f32; 3]);

impl LinearColor {
    #[cfg(test)]
    pub fn rgb(self) -> [f32; 3] {
        self.0
    }

    pub fn a(self, alpha: f32) -> [f32; 4] {
        let [r, g, b] = self.0;
        [r, g, b, alpha]
    }

    fn mix(self, other: Self, amount: f32) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        let [ar, ag, ab] = self.0;
        let [br, bg, bb] = other.0;
        Self([
            ar + (br - ar) * amount,
            ag + (bg - ag) * amount,
            ab + (bb - ab) * amount,
        ])
    }
}

/// The accent being shown this frame, packed back into the 0xRRGGBB that an
/// authored [`Color`] is written as.
///
/// For the one place a colour of this shell's has to leave the process: the mark
/// around a selected floating window is painted by the compositor, because such
/// a window is drawn in front of every surface this shell owns. Packed rather
/// than handed over as three numbers because that is what a colour *is* on the
/// page it was authored on, and a palette written down a second way is a palette
/// that can disagree with itself.
///
/// The shown accent rather than the applied one, so a mark on screen while
/// somebody is trying accents on the Settings page travels with everything else
/// that is coloured by it — see [`theme`].
pub fn shown_accent() -> u32 {
    let LinearColor([red, green, blue]) = theme().accent;
    let byte = |channel: f32| (srgb(channel.clamp(0.0, 1.0)) * 255.0).round() as u32;
    (byte(red) << 16) | (byte(green) << 8) | byte(blue)
}

/// The sRGB transfer function, inverted.
fn linear(channel: f32) -> f32 {
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

/// And the way back out of linear light, for the one colour that has to be
/// written down again as it was authored. See [`shown_accent`].
fn srgb(channel: f32) -> f32 {
    if channel <= 0.003_130_8 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    }
}

/// Everything the shell is allowed to be coloured with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// The colour of being chosen. Selections, focused rims, the bloom behind
    /// the item under the cursor.
    pub accent: Color,
    /// A lighter cast of it, for the lit edge of a selected pane — glass
    /// catches light brighter than its own tint.
    pub accent_soft: Color,
    /// Deep enough to sit *under* content as a fill.
    pub accent_deep: Color,
    /// The neutral tint of a glass pane: what the wallpaper behind it is
    /// stained with. Nearly black, so labels stay legible over anything.
    pub glass: Color,
    /// The tint of a control sitting on a pane — lighter, because glass over
    /// glass reads as frostier, not darker.
    pub glass_raised: Color,
    /// The specular highlight along a lit edge. Not pure white: a faintly warm
    /// grey reads as light rather than as a drawn line.
    pub rim: Color,
    pub text: Color,
    pub text_soft: Color,
    /// The one warning colour, for choices that end the session.
    pub danger: Color,
    /// The wallpaper's gradient, as two pairs the mood drifts between:
    /// `[top, bottom, other top, other bottom]`.
    pub sky: [Color; 4],
    /// The soft lift behind the bar's cross point, so the icons have
    /// something to sit on.
    pub glow: Color,
}

/// One coherent frame of the palette, after authored sRGB colours have become
/// linear light and any accent transition has been applied.
///
/// The UI and the wallpaper both read this snapshot. Interpolating the whole
/// structure is what keeps glass, selection light and sky in the same theme
/// throughout a change rather than letting one jump ahead of the others.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderedTheme {
    pub accent: LinearColor,
    pub accent_soft: LinearColor,
    pub accent_deep: LinearColor,
    pub glass: LinearColor,
    pub glass_raised: LinearColor,
    pub rim: LinearColor,
    pub text: LinearColor,
    pub text_soft: LinearColor,
    pub danger: LinearColor,
    pub sky: [LinearColor; 4],
    pub glow: LinearColor,
}

impl Theme {
    fn rendered(self) -> RenderedTheme {
        RenderedTheme {
            accent: self.accent.linear(),
            accent_soft: self.accent_soft.linear(),
            accent_deep: self.accent_deep.linear(),
            glass: self.glass.linear(),
            glass_raised: self.glass_raised.linear(),
            rim: self.rim.linear(),
            text: self.text.linear(),
            text_soft: self.text_soft.linear(),
            danger: self.danger.linear(),
            sky: self.sky.map(Color::linear),
            glow: self.glow.linear(),
        }
    }
}

impl RenderedTheme {
    fn mix(self, other: Self, amount: f32) -> Self {
        Self {
            accent: self.accent.mix(other.accent, amount),
            accent_soft: self.accent_soft.mix(other.accent_soft, amount),
            accent_deep: self.accent_deep.mix(other.accent_deep, amount),
            glass: self.glass.mix(other.glass, amount),
            glass_raised: self.glass_raised.mix(other.glass_raised, amount),
            rim: self.rim.mix(other.rim, amount),
            text: self.text.mix(other.text, amount),
            text_soft: self.text_soft.mix(other.text_soft, amount),
            danger: self.danger.mix(other.danger, amount),
            sky: std::array::from_fn(|index| self.sky[index].mix(other.sky[index], amount)),
            glow: self.glow.mix(other.glow, amount),
        }
    }
}

// There was a second warm colour here once — a fixed deep red, outside every
// palette, for the fill behind a Yes that destroys something. The argument for
// it was that a confirmation's whole content is the difference between its two
// answers, so they must not be told apart by a colour that under the red accent
// is also the colour of being highlighted.
//
// It went because the premise was wrong. The two answers are told apart by
// their order — the harmless one first, and the one the question opens standing
// on — and by which of them the highlight is on, which is the same thing the
// user reads everywhere else in the shell. A button already lit before anybody
// has chosen it is a panel answering its own question. [`Theme::danger`], on
// the light, is now the only warning colour, and it is the same one on a menu
// row as on an answer.

/// Violet on a deep indigo night. The shell's own colour, and the one a first
/// run comes up in.
pub const PURPLE: Theme = Theme {
    accent: Color(0x8B5CF6),
    accent_soft: Color(0xC4B5FD),
    accent_deep: Color(0x4C1D95),
    glass: Color(0x140B26),
    glass_raised: Color(0xB9A7E8),
    rim: Color(0xFFFFFF),
    text: Color(0xFFFFFF),
    text_soft: Color(0xD6CBEF),
    danger: Color(0xE0533A),
    sky: [
        Color(0x1B1140),
        Color(0x060314),
        Color(0x2A1252),
        Color(0x0A0620),
    ],
    glow: Color(0x5B21B6),
};

/// The same night, turned to blue.
///
/// Every colour of it is the violet one carried round the wheel at the same
/// lightness — the wallpaper's four sky colours included, because the ribbons
/// and the aurora in it are drawn *in* the accent (see `shaders.wgsl`), and an
/// accent that no longer belongs to the sky behind it reads as two themes
/// fighting rather than as one. What stays put is what was never violet to
/// begin with: white light, white text, and the one warning colour.
pub const BLUE: Theme = Theme {
    accent: Color(0x3B82F6),
    accent_soft: Color(0x93C5FD),
    accent_deep: Color(0x1E3A8A),
    glass: Color(0x081026),
    glass_raised: Color(0xA7C6E8),
    rim: Color(0xFFFFFF),
    text: Color(0xFFFFFF),
    text_soft: Color(0xCBDDEF),
    danger: Color(0xE0533A),
    sky: [
        Color(0x0F1B40),
        Color(0x030614),
        Color(0x122A52),
        Color(0x060A20),
    ],
    glow: Color(0x1E40AF),
};

/// Green light through a very dark forest glass.
///
/// The main rung is held below the much brighter canonical green used for
/// flat buttons: this shell adds it through glass and through the wallpaper,
/// where a brighter green would wash the white labels and glossy wave crests.
pub const GREEN: Theme = Theme {
    accent: Color(0x16A34A),
    accent_soft: Color(0x91D39F),
    accent_deep: Color(0x14532D),
    glass: Color(0x06170D),
    glass_raised: Color(0xA7C8B1),
    rim: Color(0xFFFFFF),
    text: Color(0xFFFFFF),
    text_soft: Color(0xC7DFCE),
    danger: Color(0xE0533A),
    sky: [
        Color(0x082114),
        Color(0x020A05),
        Color(0x0A3018),
        Color(0x030F08),
    ],
    glow: Color(0x14532D),
};

/// Gold-yellow on an amber-black sky.
///
/// Yellow carries far more visible light than violet at the same numerical
/// value, so this is a deep gold rather than a near-white lemon. It remains
/// unmistakably yellow while leaving headroom for the shell's pale speculars.
pub const YELLOW: Theme = Theme {
    accent: Color(0xCA8A04),
    accent_soft: Color(0xDFBC73),
    accent_deep: Color(0x713F12),
    glass: Color(0x151002),
    glass_raised: Color(0xD8C389),
    rim: Color(0xFFFFFF),
    text: Color(0xFFFFFF),
    text_soft: Color(0xE9DFC3),
    danger: Color(0xE0533A),
    sky: [
        Color(0x292008),
        Color(0x0D0A02),
        Color(0x382B0A),
        Color(0x151003),
    ],
    glow: Color(0x713F12),
};

/// Red glass, with destructive choices kept amber so ordinary selection and
/// an action that ends the session never become the same signal.
pub const RED: Theme = Theme {
    accent: Color(0xEF4444),
    accent_soft: Color(0xFCA5A5),
    accent_deep: Color(0x7F1D1D),
    glass: Color(0x230707),
    glass_raised: Color(0xD8A7A7),
    rim: Color(0xFFFFFF),
    text: Color(0xFFFFFF),
    text_soft: Color(0xE9CCCC),
    danger: Color(0xF59E0B),
    sky: [
        Color(0x3A0D0D),
        Color(0x100202),
        Color(0x501010),
        Color(0x1E0505),
    ],
    glow: Color(0x991B1B),
};

/// Teal water: the one colour on the wheel between the green and the blue.
///
/// It has to stay there. Pulled warmer it becomes the green and pulled cooler
/// the blue, and an accent that reads as one of its neighbours is a row in the
/// setting that changes nothing.
pub const TEAL: Theme = Theme {
    accent: Color(0x14B8A6),
    accent_soft: Color(0x5EEAD4),
    accent_deep: Color(0x134E4A),
    glass: Color(0x051A19),
    glass_raised: Color(0xA7CFC9),
    rim: Color(0xFFFFFF),
    text: Color(0xFFFFFF),
    text_soft: Color(0xC7E0DB),
    danger: Color(0xE0533A),
    sky: [
        Color(0x08302B),
        Color(0x020F0D),
        Color(0x0B4038),
        Color(0x041614),
    ],
    glow: Color(0x115E59),
};

/// Pale periwinkle over a night with most of its colour taken out.
///
/// The light one. Purple and Blue are saturated colours at two thirds
/// lightness; this is a much paler rung on a far greyer sky, so what the eye
/// reads first is not the hue — which sits between theirs — but how much light
/// the accent carries.
pub const INDIGO: Theme = Theme {
    accent: Color(0xA5B4FC),
    accent_soft: Color(0xC7D2FE),
    accent_deep: Color(0x312E81),
    glass: Color(0x0D0E22),
    glass_raised: Color(0xB3B9DE),
    rim: Color(0xFFFFFF),
    text: Color(0xFFFFFF),
    text_soft: Color(0xD5D9F2),
    danger: Color(0xE0533A),
    sky: [
        Color(0x101128),
        Color(0x030409),
        Color(0x15163A),
        Color(0x070813),
    ],
    glow: Color(0x252270),
};

/// Magenta glass, with the warning colour left where it always is.
///
/// Pink and the one warning colour are far enough apart on the wheel — magenta
/// against a warm red-orange — that a destructive answer stays legible as one
/// without the swap Red has to make.
pub const PINK: Theme = Theme {
    accent: Color(0xEC4899),
    accent_soft: Color(0xF9A8D4),
    accent_deep: Color(0x831843),
    glass: Color(0x200714),
    glass_raised: Color(0xE0A7C4),
    rim: Color(0xFFFFFF),
    text: Color(0xFFFFFF),
    text_soft: Color(0xF0CCE0),
    danger: Color(0xE0533A),
    sky: [
        Color(0x2C0A1E),
        Color(0x0C0207),
        Color(0x3E0D28),
        Color(0x160410),
    ],
    glow: Color(0x861042),
};

/// Orange on a scorched black sky, with destructive choices turned pure red.
///
/// The one warning colour is itself a red-orange, which under this accent would
/// be a slightly duller cast of the colour everything chosen is already drawn
/// in. Red answers the same collision by going the other way, to amber; here
/// the answer is a red with no orange left in it.
pub const ORANGE: Theme = Theme {
    accent: Color(0xF97316),
    accent_soft: Color(0xFDBA74),
    accent_deep: Color(0x7C2D12),
    glass: Color(0x1D0B02),
    glass_raised: Color(0xE8C3A7),
    rim: Color(0xFFFFFF),
    text: Color(0xFFFFFF),
    text_soft: Color(0xF0DAC4),
    danger: Color(0xDC2626),
    sky: [
        Color(0x301206),
        Color(0x0D0402),
        Color(0x421A07),
        Color(0x160702),
    ],
    glow: Color(0x842C0F),
};

/// Not white: the palest grey that is still a colour of its own.
///
/// Pure white is what the rim and the text already are, and an accent equal to
/// them would leave a selection with nothing to be brighter than. This sits one
/// rung below, so a lit edge still lifts off the thing it is lighting.
pub const WHITE: Theme = Theme {
    accent: Color(0xE4E4E4),
    accent_soft: Color(0xF5F5F5),
    accent_deep: Color(0x3D3D3D),
    glass: Color(0x0C0C0C),
    glass_raised: Color(0xC4C4C4),
    rim: Color(0xFFFFFF),
    text: Color(0xFFFFFF),
    text_soft: Color(0xC9C9C9),
    danger: Color(0xE0533A),
    sky: [
        Color(0x151515),
        Color(0x040404),
        Color(0x1E1E1E),
        Color(0x090909),
    ],
    glow: Color(0x444444),
};

/// Mid grey on neutral dark grey: the monochrome one.
///
/// The middle of the three colourless palettes, and the only one whose accent
/// stands well clear of both the white light on its edges and the dark behind
/// it. Nothing in it is tinted — a grey with a cast in it would be one more
/// colour pretending to be no colour at all.
pub const SILVER: Theme = Theme {
    accent: Color(0xA6A6A6),
    accent_soft: Color(0xD6D6D6),
    accent_deep: Color(0x4A4A4A),
    glass: Color(0x121212),
    glass_raised: Color(0xC0C0C0),
    rim: Color(0xFFFFFF),
    text: Color(0xFFFFFF),
    text_soft: Color(0xCFCFCF),
    danger: Color(0xE0533A),
    sky: [
        Color(0x2A2A2A),
        Color(0x0B0B0B),
        Color(0x383838),
        Color(0x131313),
    ],
    glow: Color(0x6B6B6B),
};

/// Near black, for a screen that should say as little as it can.
///
/// Near rather than at: a selection has to be *some* light, so the accent is a
/// dark grey and the sky keeps a hair of tone above nothing at all. What tells
/// a chosen row from the rest here is mostly the white rim — which is what does
/// it in every palette; this one simply leaves it to work alone.
pub const BLACK: Theme = Theme {
    accent: Color(0x525252),
    accent_soft: Color(0x8A8A8A),
    accent_deep: Color(0x1F1F1F),
    glass: Color(0x0A0A0A),
    glass_raised: Color(0x969696),
    rim: Color(0xFFFFFF),
    text: Color(0xFFFFFF),
    text_soft: Color(0xB0B0B0),
    danger: Color(0xE0533A),
    sky: [
        Color(0x141414),
        Color(0x030303),
        Color(0x1C1C1C),
        Color(0x070707),
    ],
    glow: Color(0x2E2E2E),
};

/// A palette under the name the user picks it by.
///
/// The name is the setting: it is what the Settings column shows, and what
/// goes in the config file. An index would be smaller and would break the
/// moment a palette is inserted in the middle of the list.
pub struct Accent {
    pub name: &'static str,
    pub theme: Theme,
}

/// Every accent the shell can be set to, in the order the setting lists them.
///
/// The first is the default, which is what an unreadable or unrecognised
/// setting falls back to.
pub const ACCENTS: &[Accent] = &[
    Accent {
        name: "Purple",
        theme: PURPLE,
    },
    Accent {
        name: "Blue",
        theme: BLUE,
    },
    Accent {
        name: "Green",
        theme: GREEN,
    },
    Accent {
        name: "Yellow",
        theme: YELLOW,
    },
    Accent {
        name: "Red",
        theme: RED,
    },
    Accent {
        name: "Teal",
        theme: TEAL,
    },
    Accent {
        name: "Indigo",
        theme: INDIGO,
    },
    Accent {
        name: "Pink",
        theme: PINK,
    },
    Accent {
        name: "Orange",
        theme: ORANGE,
    },
    Accent {
        name: "White",
        theme: WHITE,
    },
    Accent {
        name: "Silver",
        theme: SILVER,
    },
    Accent {
        name: "Black",
        theme: BLACK,
    },
];

/// Long enough to see the colour flow through the glass and wallpaper, short
/// enough to keep up while the user walks the list.
const ACCENT_CHANGE: f32 = 0.36;

#[derive(Debug, Clone, Copy)]
struct AccentTransition {
    /// Which of [`ACCENTS`] owns the Settings tick and saved value. This is
    /// deliberately separate from the palette merely being previewed, but it
    /// shares the same lock so a future input thread cannot split the two.
    applied: usize,
    target: usize,
    from: RenderedTheme,
    to: RenderedTheme,
    shown: RenderedTheme,
    progress: f32,
}

impl AccentTransition {
    fn settled(index: usize) -> Self {
        let shown = ACCENTS[index].theme.rendered();
        Self {
            applied: index,
            target: index,
            from: shown,
            to: shown,
            shown,
            progress: 1.0,
        }
    }
}

static TRANSITION: OnceLock<Mutex<AccentTransition>> = OnceLock::new();

fn transition() -> &'static Mutex<AccentTransition> {
    TRANSITION.get_or_init(|| Mutex::new(AccentTransition::settled(0)))
}

fn lock_transition() -> MutexGuard<'static, AccentTransition> {
    transition().lock().unwrap_or_else(|held| held.into_inner())
}

fn accent_index(name: &str) -> Option<usize> {
    ACCENTS
        .iter()
        .position(|accent| accent.name.eq_ignore_ascii_case(name))
}

/// Point the displayed palette at `index`, beginning from the exact blend on
/// screen now. Retargeting a half-finished preview therefore changes course
/// without jumping back to either endpoint.
fn transition_to(transition: &mut AccentTransition, index: usize) -> bool {
    if transition.target == index {
        return false;
    }

    let to = ACCENTS[index].theme.rendered();
    if transition.shown == to {
        let applied = transition.applied;
        *transition = AccentTransition::settled(index);
        transition.applied = applied;
        return false;
    }

    transition.target = index;
    transition.from = transition.shown;
    transition.to = to;
    transition.progress = 0.0;
    true
}

/// The applied accent, name and authored palette together.
pub fn accent() -> &'static Accent {
    // Clamped rather than indexed: nothing can set this out of range today,
    // and a palette that came back as a panic would take the session with it.
    let index = lock_transition().applied.min(ACCENTS.len() - 1);
    &ACCENTS[index]
}

/// The palette displayed this frame, including an in-flight preview.
///
/// Returned by value so one immutable linear-light snapshot can represent a
/// point between two authored [`Theme`] constants. It changes only when
/// [`animate`] advances it, so every surface rendered in one shell frame sees
/// precisely the same colours.
pub fn theme() -> RenderedTheme {
    lock_transition().shown
}

/// Set the applied and displayed accent immediately.
///
/// This is the startup path: the saved setting is read before a first frame
/// exists to animate. Runtime choices use [`commit_accent`] instead, so an
/// already-running preview never snaps at the moment it is applied.
///
/// Names are matched without regard to case, so `blue` in a file the user
/// typed is the same setting as the `Blue` the shell writes back.
pub fn set_accent(name: &str) -> bool {
    let Some(index) = accent_index(name) else {
        return false;
    };
    *lock_transition() = AccentTransition::settled(index);
    true
}

/// Animate towards an accent without applying or saving it.
pub fn preview_accent(name: &str) -> bool {
    let Some(index) = accent_index(name) else {
        return false;
    };
    transition_to(&mut lock_transition(), index);
    true
}

/// Make an accent the applied setting while preserving any preview already
/// travelling towards it.
pub fn commit_accent(name: &str) -> bool {
    let Some(index) = accent_index(name) else {
        return false;
    };
    let mut transition = lock_transition();
    transition.applied = index;
    transition_to(&mut transition, index);
    true
}

// --- the theme: which material each half of the shell is drawn in -----------
//
// The accent above is a colour and it *travels*: a preview flows towards the
// highlighted palette and back again, because a colour between two colours is a
// colour. A material cannot be halfway. The band of water and the glass-silk
// lines are different geometry, and a glyph is either a bead with a bevel and a
// shadow or the flat shape of itself; there is nothing in between to show for
// half a second. So this one lands whole, on the frame it is chosen — or
// highlighted, since seeing it is the whole reason a settings row previews.
//
// Two of them, and they are genuinely independent. The wallpaper is one
// evaluation of a long function for every pixel on every screen, every frame;
// a mark is a few dozen pixels of a settings row. They are not the same
// expense and there is no reason a machine should have to answer for both at
// once — nor any reason somebody who simply likes flat marks should have to
// give up the water to get them.

/// Which half of the Theme setting a material belongs to.
///
/// The two halves are the same question asked about two different things, so
/// everything below takes one of these rather than existing twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// The picture behind everything: the band of water, the glass-silk ribbons
    /// this shell drew before it, or a picture or film of the user's own put
    /// there instead of either.
    ///
    /// The one half with a third answer, and it is a third answer of a different
    /// kind — see [`Part::styles`].
    Wallpaper,
    /// Every mark the shell draws itself: a bead of water shaded out of its own
    /// distance field, or the flat shape of one.
    Icons,
}

/// Both halves, in the order Settings lists them: the wallpaper first, because
/// it is the whole screen and the more expensive of the two.
pub const PARTS: [Part; 2] = [Part::Wallpaper, Part::Icons];

impl Part {
    /// What Settings titles the row with.
    pub fn title(self) -> &'static str {
        match self {
            Self::Wallpaper => crate::i18n::text("shell-wallpaper"),
            Self::Icons => crate::i18n::text("shell-icons"),
        }
    }

    /// The key this half is written to `shell.toml` under.
    ///
    /// Here rather than in `settings`, because the compositor and the display
    /// manager read these keys out of the same file and the shell must not be
    /// the only place that knows what they are called.
    pub fn key(self) -> &'static str {
        match self {
            Self::Wallpaper => "theme-wallpaper",
            Self::Icons => "theme-icons",
        }
    }

    /// The values this half may be set to, in the order Settings lists them.
    ///
    /// The two materials for either of them, and one more for the wallpaper:
    /// the user's own picture, which is not a material at all but a file
    /// standing where the scene would be. There is nothing it could mean about
    /// a mark — see [`wallpaper::WALLPAPER_STYLES`] — and this is the one place
    /// that says so, so that setting, previewing and saving cannot each come to
    /// their own conclusion about what the domain is.
    pub fn styles(self) -> &'static [&'static str] {
        match self {
            Self::Wallpaper => &wallpaper::WALLPAPER_STYLES,
            Self::Icons => &wallpaper::STYLES,
        }
    }
}

/// The material one half of the shell draws in, and the one it will go back to
/// when a preview is abandoned.
///
/// Two of them rather than one for the same reason the accent keeps two: the
/// cursor walking down a list of values shows each of them, and walking back off
/// the list has to undo that without having written anything down.
#[derive(Debug, Clone, Copy, Default)]
struct Chosen {
    applied: Style,
    shown: Style,
}

/// What the shell is made of: one answer for the picture behind it and one for
/// the marks on top of it — and, kept with them because the Theme page previews
/// and restores them together, whether the current carries its sparkles.
#[derive(Debug, Clone, Copy, Default)]
struct Material {
    wallpaper: Chosen,
    icons: Chosen,
    particles: Particles,
}

impl Material {
    fn part(&mut self, part: Part) -> &mut Chosen {
        match part {
            Part::Wallpaper => &mut self.wallpaper,
            Part::Icons => &mut self.icons,
        }
    }
}

static MATERIAL: OnceLock<Mutex<Material>> = OnceLock::new();

fn lock_material() -> MutexGuard<'static, Material> {
    MATERIAL
        .get_or_init(|| Mutex::new(Material::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The material to draw this half of the frame with, preview included.
pub fn style(part: Part) -> Style {
    lock_material().part(part).shown
}

/// The material the user has actually chosen for it, which is what gets written
/// down.
pub fn applied_style(part: Part) -> Style {
    lock_material().part(part).applied
}

/// What the shader is told, which is the one number each half of the theme comes
/// down to on the GPU: nought for the shell's own material and one for the plain
/// one.
pub fn style_flag(part: Part) -> f32 {
    flag(style(part))
}

/// The same number for a material named outright rather than looked up.
///
/// Two callers, and the second is why this is not written inside
/// [`style_flag`]: a row on the Theme page carries the material it *applies*
/// down to the quad its mark is drawn on — see [`crate::gpu::Quad::mark`] —
/// and the number that says which material has to be the same number in both
/// places or a row would preview one thing and set another.
pub fn flag(style: Style) -> f32 {
    match style {
        Style::Default => 0.0,
        Style::Simple => 1.0,
        // The one value that is not a material: the shader stops drawing the
        // scene at all and reads the picture the shell put in front of it. The
        // wallpaper's lane only — nothing ever sets this half for the marks; see
        // [`Part::styles`].
        Style::Custom => 2.0,
    }
}

/// Set a material outright, applied and shown together. The startup path,
/// where the saved setting is read before there is a frame to answer with.
///
/// Names are matched exactly, as [`wallpaper::style`] matches them, and against
/// the values *this half* offers: the spellings [`Part::styles`] lists are the
/// whole of the setting's domain, and Custom wallpaper is not one of the marks'.
pub fn set_style(part: Part, name: &str) -> bool {
    let known = part.styles().contains(&name);
    let style = wallpaper::style(name);
    let mut material = lock_material();
    let chosen = material.part(part);
    chosen.applied = style;
    chosen.shown = style;
    known
}

/// Draw in a material without choosing it, for a highlighted row.
pub fn preview_style(part: Part, name: &str) -> bool {
    if !part.styles().contains(&name) {
        return false;
    }
    lock_material().part(part).shown = wallpaper::style(name);
    true
}

/// Choose the material the shell is showing for that half.
pub fn commit_style(part: Part, name: &str) -> bool {
    if !part.styles().contains(&name) {
        return false;
    }
    let style = wallpaper::style(name);
    let mut material = lock_material();
    let chosen = material.part(part);
    chosen.applied = style;
    chosen.shown = style;
    true
}

/// Leave a preview behind and go back to the materials the user applied.
///
/// Both halves at once, and the particles with them, and deliberately: this is
/// what a cursor leaving the Theme rows calls, and it cannot know which of them
/// it walked through.
pub fn restore_style() {
    let mut material = lock_material();
    for part in PARTS {
        let chosen = material.part(part);
        chosen.shown = chosen.applied;
    }
    material.particles.shown = material.particles.applied;
}

// --- the particles: whether the current carries its sparkles ----------------
//
// Beside the materials rather than one of them, because it is not a material:
// the sparkles are light the current carries, drawn the same over the water and
// over the fine ribbons, and the question about them is simply whether they are
// there. It lands whole and previews the way the materials beside it do —
// highlighting Off takes them off the screen, and walking away puts them back.

/// Whether the sparkles are drawn, and whether they will be once a preview is
/// abandoned. On until somebody turns them off.
#[derive(Debug, Clone, Copy)]
struct Particles {
    applied: bool,
    shown: bool,
}

impl Default for Particles {
    fn default() -> Self {
        Self {
            applied: true,
            shown: true,
        }
    }
}

/// Whether the current carries its sparkles this frame, preview included.
pub fn particles() -> bool {
    lock_material().particles.shown
}

/// Whether the user has them on, which is what gets written down.
pub fn applied_particles() -> bool {
    lock_material().particles.applied
}

/// What the shader is told: one where the sparkles are drawn, and nought where
/// they are not — the number every writer of the uniform that has never heard
/// of them already sends, so a writer has to ask this to draw them.
pub fn particles_flag() -> f32 {
    if particles() {
        1.0
    } else {
        0.0
    }
}

/// Set them outright, applied and shown together. The startup path.
pub fn set_particles(on: bool) {
    lock_material().particles = Particles {
        applied: on,
        shown: on,
    };
}

/// Draw them, or not, without choosing it, for a highlighted row.
pub fn preview_particles(on: bool) {
    lock_material().particles.shown = on;
}

/// Choose whether they are drawn.
pub fn commit_particles(on: bool) {
    set_particles(on);
}

/// Leave preview behind and flow back to the last accent the user applied.
pub fn restore_accent() {
    let mut transition = lock_transition();
    let applied = transition.applied.min(ACCENTS.len() - 1);
    transition_to(&mut transition, applied);
}

/// Advance the displayed palette once for a shell frame.
///
/// Returns `true` while another frame is needed. The cubic ease has zero
/// velocity at both ends, and interpolation happens between linear-light
/// colours rather than between packed sRGB integers.
pub fn animate(dt: f32) -> bool {
    let mut transition = lock_transition();
    if transition.progress >= 1.0 {
        return false;
    }
    if !dt.is_finite() || dt <= 0.0 {
        return true;
    }

    transition.progress = (transition.progress + dt / ACCENT_CHANGE).min(1.0);
    let p = transition.progress;
    let eased = p * p * (3.0 - 2.0 * p);
    transition.shown = transition.from.mix(transition.to, eased);
    if transition.progress >= 1.0 {
        // Keep the authored endpoint exact rather than the result of floating
        // point arithmetic that only ought to be the same value.
        transition.shown = transition.to;
        false
    } else {
        true
    }
}

/// Run `body` with `name` as the accent, and put back whatever was in force.
///
/// The accent is process-wide and the test binary is threaded, so a test that
/// changed it and left would be a test that broke whichever other one happened
/// to be reading the palette at the time. Everything that touches the setting
/// goes through here, which serialises them against each other. The lock is
/// taken through its own poison, since a test that panics while holding it has
/// already reported the failure that matters.
#[cfg(test)]
struct AccentRestore(&'static str);

#[cfg(test)]
impl Drop for AccentRestore {
    fn drop(&mut self) {
        set_accent(self.0);
    }
}

#[cfg(test)]
pub fn with_accent<T>(name: &str, body: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _held = LOCK.lock().unwrap_or_else(|held| held.into_inner());
    let restore = AccentRestore(accent().name);
    assert!(set_accent(name), "no accent is called {name}");
    let out = body();
    drop(restore);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sparkles are part of the look a person starts with: a shell that has
    /// read no settings at all draws the current with them.
    #[test]
    fn the_particles_are_on_until_somebody_turns_them_off() {
        let material = Material::default();
        assert!(material.particles.applied);
        assert!(material.particles.shown);
    }

    /// The conversion is the whole point of authoring in hex: a mid grey must
    /// arrive at the GPU as mid *light*, which is a much lower number.
    #[test]
    fn hex_is_converted_out_of_srgb() {
        let [r, g, b] = Color(0x808080).rgb();
        assert!((r - 0.2158).abs() < 1e-3, "{r}");
        assert_eq!(r, g);
        assert_eq!(g, b);

        assert_eq!(Color(0x000000).rgb(), [0.0; 3]);
        assert_eq!(Color(0xFFFFFF).rgb(), [1.0; 3]);
    }

    /// The one colour of this shell's that leaves the process comes back out as
    /// the colour it was authored as.
    ///
    /// It has to: the mark around a selected floating window is painted by the
    /// compositor, and a mark that was a shade off the accent everything beside
    /// it is drawn in would be the seam this palette exists to prevent. The trip
    /// out and back is through linear light, so it is a real conversion and not
    /// a number handed along.
    #[test]
    fn the_accent_that_crosses_the_protocol_is_the_accent_that_was_authored() {
        for accent in ACCENTS {
            with_accent(accent.name, || {
                assert_eq!(
                    shown_accent(),
                    accent.theme.accent.0,
                    "{} came back as {:#08x}",
                    accent.name,
                    shown_accent()
                );
            });
        }
    }

    #[test]
    fn channels_land_in_the_right_order() {
        let [r, g, b] = Color(0xFF0000).rgb();
        assert_eq!((r, g, b), (1.0, 0.0, 0.0));
        let [r, g, b] = Color(0x0000FF).rgb();
        assert_eq!((r, g, b), (0.0, 0.0, 1.0));
    }

    #[test]
    fn alpha_rides_along_untouched() {
        assert_eq!(Color(0xFFFFFF).a(0.42)[3], 0.42);
    }

    /// The compositor draws this same wallpaper before the shell exists, from
    /// the shared table in `lxb-protocol::wallpaper`. That is what turns the
    /// interval before the first shell frame from a black screen into the
    /// picture the login screen was already showing — and it only works while
    /// the two palettes are the same palette. This is the test that says so:
    /// change an accent here and it fails until the shared table follows.
    #[test]
    fn the_compositor_draws_the_same_palette_before_the_shell_starts() {
        assert_eq!(ACCENTS.len(), lxb_protocol::wallpaper::PALETTES.len());
        for (accent, shared) in ACCENTS.iter().zip(lxb_protocol::wallpaper::PALETTES) {
            assert_eq!(accent.name, shared.name);
            let theme = accent.theme;
            for (mine, theirs) in theme.sky.iter().zip(&shared.sky) {
                assert_eq!(mine.0, theirs.0, "{} sky", accent.name);
            }
            assert_eq!(theme.accent.0, shared.accent[0].0, "{}", accent.name);
            assert_eq!(theme.accent_soft.0, shared.accent[1].0, "{}", accent.name);
            assert_eq!(theme.accent_deep.0, shared.accent[2].0, "{}", accent.name);
            assert_eq!(theme.glow.0, shared.glow.0, "{} glow", accent.name);
        }
    }

    /// An accent is a whole palette under a name, and the names are what the
    /// setting is stored and shown as — so two of them cannot be the same, and
    /// two palettes cannot be either, or the setting would have a row in it
    /// that changes nothing.
    #[test]
    fn each_accent_is_a_distinct_palette_under_a_distinct_name() {
        assert_eq!(
            ACCENTS.iter().map(|accent| accent.name).collect::<Vec<_>>(),
            [
                "Purple", "Blue", "Green", "Yellow", "Red", "Teal", "Indigo", "Pink", "Orange",
                "White", "Silver", "Black"
            ]
        );

        for (index, accent) in ACCENTS.iter().enumerate() {
            assert!(!accent.name.is_empty());
            for other in &ACCENTS[index + 1..] {
                assert_ne!(
                    accent.name.to_lowercase(),
                    other.name.to_lowercase(),
                    "two accents answer to the same name"
                );
                assert_ne!(
                    accent.theme.accent, other.theme.accent,
                    "{} and {} are the same colour",
                    accent.name, other.name
                );
                assert_ne!(accent.theme.sky[0], other.theme.sky[0]);
            }
        }
    }

    /// The setting is the palette: everything that asks for a colour after it
    /// changes gets the new one, including the wallpaper's own four.
    #[test]
    fn setting_the_accent_changes_what_the_shell_draws_with() {
        with_accent("Blue", || {
            assert_eq!(accent().name, "Blue");
            assert_eq!(theme(), BLUE.rendered());
        });
        with_accent("Purple", || {
            assert_eq!(theme(), PURPLE.rendered());

            // A file the user typed, and one they got wrong. The first is the
            // setting; the second leaves the shell as it was rather than
            // dropping it into some other palette.
            assert!(set_accent("blue"));
            assert_eq!(theme(), BLUE.rendered());
            assert!(!set_accent("Chartreuse"));
            assert_eq!(theme(), BLUE.rendered());
        });
    }

    #[test]
    fn preview_flows_without_changing_the_applied_accent_and_restores() {
        with_accent("Purple", || {
            let purple = theme();
            assert!(preview_accent("Green"));
            assert_eq!(accent().name, "Purple", "preview is not Apply");
            assert_eq!(theme(), purple, "targeting alone cannot jump a frame");

            assert!(animate(ACCENT_CHANGE / 2.0));
            let halfway = theme();
            assert_ne!(halfway, purple);
            assert_ne!(halfway, GREEN.rendered());
            assert!(!animate(ACCENT_CHANGE));
            assert_eq!(theme(), GREEN.rendered());
            assert_eq!(accent().name, "Purple");

            restore_accent();
            assert_eq!(theme(), GREEN.rendered(), "restoring also starts smoothly");
            assert!(!animate(ACCENT_CHANGE));
            assert_eq!(theme(), purple);

            assert!(preview_accent("Yellow"));
            assert!(animate(ACCENT_CHANGE / 2.0));
            let partial_preview = theme();
            restore_accent();
            assert_eq!(
                theme(),
                partial_preview,
                "Back halfway through a preview cannot jump"
            );
            assert!(!animate(ACCENT_CHANGE));
            assert_eq!(theme(), purple);
        });
    }

    #[test]
    fn a_new_preview_continues_from_the_blend_already_on_screen() {
        with_accent("Purple", || {
            assert!(preview_accent("Green"));
            assert!(animate(ACCENT_CHANGE / 2.0));
            let before_retarget = theme();

            assert!(preview_accent("Red"));
            assert_eq!(
                theme(),
                before_retarget,
                "walking the list cannot jump back to a palette endpoint"
            );
            assert!(!animate(ACCENT_CHANGE));
            assert_eq!(theme(), RED.rendered());
        });
    }

    #[test]
    fn reversing_before_a_frame_cannot_turn_a_preview_into_apply() {
        with_accent("Purple", || {
            assert!(preview_accent("Green"));
            assert!(!animate(ACCENT_CHANGE));
            assert_eq!(theme(), GREEN.rendered());
            assert_eq!(accent().name, "Purple");

            // The first move points away, but no frame advances before the
            // second comes back to the palette already on screen. Settling
            // that display target must preserve the separately applied one.
            assert!(preview_accent("Red"));
            assert!(preview_accent("Green"));
            assert_eq!(theme(), GREEN.rendered());
            assert_eq!(accent().name, "Purple", "preview is still not Apply");

            restore_accent();
            assert!(!animate(ACCENT_CHANGE));
            assert_eq!(theme(), PURPLE.rendered());
        });
    }

    #[test]
    fn apply_keeps_the_preview_in_place_and_makes_it_the_restore_target() {
        with_accent("Purple", || {
            assert!(preview_accent("Red"));
            assert!(animate(ACCENT_CHANGE / 2.0));
            let before_apply = theme();

            assert!(commit_accent("Red"));
            assert_eq!(accent().name, "Red");
            assert_eq!(theme(), before_apply, "Apply cannot restart the flow");
            assert!(!animate(ACCENT_CHANGE));
            assert_eq!(theme(), RED.rendered());

            assert!(preview_accent("Green"));
            assert!(!animate(ACCENT_CHANGE));
            assert_eq!(accent().name, "Red");
            restore_accent();
            assert!(!animate(ACCENT_CHANGE));
            assert_eq!(theme(), RED.rendered());
        });
    }

    #[test]
    fn an_unknown_preview_or_apply_changes_nothing() {
        with_accent("Yellow", || {
            let before = theme();
            assert!(!preview_accent("Chartreuse"));
            assert!(!commit_accent("Chartreuse"));
            assert_eq!(accent().name, "Yellow");
            assert_eq!(theme(), before);
            assert!(!animate(ACCENT_CHANGE));
        });
    }

    #[test]
    fn warning_actions_stay_distinct_from_every_accent() {
        for accent in ACCENTS {
            assert_ne!(
                accent.theme.danger, accent.theme.accent,
                "{} makes an ordinary selection look destructive",
                accent.name
            );
        }
    }
}
