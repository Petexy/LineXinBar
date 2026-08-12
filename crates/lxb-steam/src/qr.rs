//! The code on the screen, as a grid of squares.
//!
//! A QR code is drawn rather than decoded here, and it is handed to the shell
//! as the modules it is made of rather than as a picture. Two reasons. The
//! shell draws everything as quads through one atlas, so a grid of booleans is
//! already in the form it draws — no image is rasterised, scaled, or uploaded.
//! And the size it should be drawn at is the panel's business, not this
//! crate's: the same code goes on a 720p television and a 4K monitor, and what
//! makes it readable to a phone camera is the module being a whole number of
//! pixels, which only the side doing the layout can arrange.

/// A code, as the squares it is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    /// How many modules along each side, quiet zone excluded.
    pub width: usize,
    /// One per module, row by row from the top left. `true` is dark.
    pub dark: Vec<bool>,
}

impl Code {
    /// Whether the module at `(x, y)` is dark. Outside the grid is light,
    /// which is what makes the quiet zone the caller draws around it correct
    /// without a second test.
    pub fn at(&self, x: usize, y: usize) -> bool {
        if x >= self.width || y >= self.width {
            return false;
        }
        self.dark.get(y * self.width + x).copied().unwrap_or(false)
    }
}

/// Encode one URL.
///
/// `None` for text no code can carry, which for the URLs Steam hands out —
/// forty-odd characters — cannot happen; it is checked rather than assumed
/// because the alternative is a panic in a shell.
pub fn encode(text: &str) -> Option<Code> {
    // Medium correction: the standard trade, and the one Steam's own app
    // draws. A code on a screen is not a code on a printed box — it is not
    // going to be scuffed — so the room that a higher level would spend on
    // redundancy is better spent on fewer, larger modules.
    let code = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::M).ok()?;
    let width = code.width();
    let dark = code
        .into_colors()
        .into_iter()
        .map(|module| module == qrcode::Color::Dark)
        .collect();
    Some(Code { width, dark })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A code of the shape Steam hands out is square, has the three finder
    /// patterns every reader looks for, and is the same code every time.
    #[test]
    fn a_challenge_url_becomes_a_readable_grid() {
        let code = encode("https://s.team/q/1/1234567890123456789").expect("a short URL");

        assert!(code.width >= 21, "smaller than the smallest QR version");
        assert_eq!(code.dark.len(), code.width * code.width, "not square");

        // The finder pattern: a 7×7 ring in each of three corners, dark on the
        // outside, light inside it, dark in the middle. Checked in the top
        // left, which is where every reader starts.
        for step in 0..7 {
            assert!(code.at(step, 0), "the top edge of the finder is broken");
            assert!(code.at(0, step), "the left edge of the finder is broken");
        }
        assert!(!code.at(1, 1), "the ring is filled in");
        assert!(code.at(3, 3), "the centre of the finder is missing");

        // Same input, same code: the panel is rebuilt on every keystroke and a
        // code that changed under the camera would never be read.
        assert_eq!(encode("https://s.team/q/1/1234567890123456789"), Some(code));
    }

    /// Outside the grid is light, so the quiet zone the panel draws around it
    /// needs no second test.
    #[test]
    fn outside_the_grid_is_light() {
        let code = encode("https://s.team/q/1/1").expect("a short URL");
        assert!(!code.at(code.width, 0));
        assert!(!code.at(0, code.width));
        assert!(!code.at(usize::MAX, usize::MAX));
    }
}
