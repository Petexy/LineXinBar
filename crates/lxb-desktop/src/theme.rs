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

/// The sRGB transfer function, inverted.
fn linear(channel: f32) -> f32 {
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
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
    /// The picture behind everything: the band of water, or the glass-silk
    /// ribbons this shell drew before it.
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
            Self::Wallpaper => "Wallpaper",
            Self::Icons => "Icons",
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
/// the marks on top of it.
#[derive(Debug, Clone, Copy, Default)]
struct Material {
    wallpaper: Chosen,
    icons: Chosen,
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
    match style(part) {
        Style::Default => 0.0,
        Style::Simple => 1.0,
    }
}

/// Set a material outright, applied and shown together. The startup path,
/// where the saved setting is read before there is a frame to answer with.
///
/// Names are matched exactly, as [`wallpaper::style`] matches them: the two
/// spellings are the whole of the setting's domain.
pub fn set_style(part: Part, name: &str) -> bool {
    let known = wallpaper::STYLES.contains(&name);
    let style = wallpaper::style(name);
    let mut material = lock_material();
    let chosen = material.part(part);
    chosen.applied = style;
    chosen.shown = style;
    known
}

/// Draw in a material without choosing it, for a highlighted row.
pub fn preview_style(part: Part, name: &str) -> bool {
    if !wallpaper::STYLES.contains(&name) {
        return false;
    }
    lock_material().part(part).shown = wallpaper::style(name);
    true
}

/// Choose the material the shell is showing for that half.
pub fn commit_style(part: Part, name: &str) -> bool {
    if !wallpaper::STYLES.contains(&name) {
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
/// Both halves at once, and deliberately: this is what a cursor leaving the
/// Theme rows calls, and it cannot know which of the two it walked through.
pub fn restore_style() {
    let mut material = lock_material();
    for part in PARTS {
        let chosen = material.part(part);
        chosen.shown = chosen.applied;
    }
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
            ["Purple", "Blue", "Green", "Yellow", "Red"]
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
