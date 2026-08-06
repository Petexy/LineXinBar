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
#[derive(Debug, Clone, Copy)]
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

/// The default: violet on a deep indigo night.
pub const DEFAULT: Theme = Theme {
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

/// The palette in force.
///
/// A function rather than the constant itself: the theme becomes a setting
/// later, and every call site is already asking rather than assuming.
pub fn theme() -> &'static Theme {
    &DEFAULT
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
}
