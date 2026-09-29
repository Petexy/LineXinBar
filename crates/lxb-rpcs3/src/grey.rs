//! A trophy's picture as a locked trophy's: the same picture, grey.
//!
//! Steam hands over a grey copy of every achievement's icon, and
//! RetroAchievements a "locked" badge, and the Trophies column shows those
//! until the thing is earned — only what somebody has done is in colour. A PS3
//! trophy set carries one picture per trophy, in colour, so the grey one is
//! made here, once, and kept beside it in the cache.

use std::io::Cursor;

/// How bright a grey picture is next to its own lightness: a little darker,
/// so that it reads as not yet earned beside the pictures in colour, as the
/// other providers' do.
const DIM: f32 = 0.8;

/// `png` without its colour, as a grey PNG with its transparency kept — or
/// `None` for something that is not a PNG this can read.
pub fn grey_png(png: &[u8]) -> Option<Vec<u8>> {
    let mut decoder = png::Decoder::new(Cursor::new(png));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut pixels).ok()?;
    let pixels = &pixels[..info.buffer_size()];
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return None,
    };
    let mut grey = Vec::with_capacity(info.width as usize * info.height as usize * 2);
    for pixel in pixels.chunks_exact(channels) {
        let (light, alpha) = match pixel {
            [y] => (f32::from(*y), 255),
            [y, a] => (f32::from(*y), *a),
            [r, g, b] => (luma(*r, *g, *b), 255),
            [r, g, b, a] => (luma(*r, *g, *b), *a),
            _ => return None,
        };
        grey.push((light * DIM).round().clamp(0.0, 255.0) as u8);
        grey.push(alpha);
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, info.width, info.height);
        encoder.set_color(png::ColorType::GrayscaleAlpha);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().ok()?;
        writer.write_image_data(&grey).ok()?;
        writer.finish().ok()?;
    }
    Some(out)
}

/// The lightness of a colour as the eye weighs its three parts (Rec. 709).
fn luma(r: u8, g: u8, b: u8) -> f32 {
    0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba_png(pixels: &[[u8; 4]], width: u32) -> Vec<u8> {
        let mut out = Vec::new();
        let height = pixels.len() as u32 / width;
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&pixels.concat()).unwrap();
        writer.finish().unwrap();
        out
    }

    /// Red, green and blue come out as three greys of their own lightness,
    /// a little dimmed, with their transparency as it was.
    #[test]
    fn a_picture_loses_its_colour_and_keeps_its_shape() {
        let source = rgba_png(
            &[
                [255, 0, 0, 255],
                [0, 255, 0, 128],
                [0, 0, 255, 0],
                [255, 255, 255, 255],
            ],
            2,
        );
        let grey = grey_png(&source).expect("a grey PNG");
        let mut reader = png::Decoder::new(Cursor::new(&grey)).read_info().unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!(info.color_type, png::ColorType::GrayscaleAlpha);
        assert_eq!((info.width, info.height), (2, 2));
        let expect = |light: f32| (light * DIM).round() as u8;
        assert_eq!(
            &pixels[..8],
            &[
                expect(0.2126 * 255.0),
                255,
                expect(0.7152 * 255.0),
                128,
                expect(0.0722 * 255.0),
                0,
                expect(255.0),
                255
            ]
        );
        assert_eq!(grey_png(b"not a png"), None);
    }
}
