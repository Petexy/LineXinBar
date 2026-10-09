//! The drawing a Windows program wears, taken out of the program.
//!
//! A row on this bar is a name and a picture. Every other column has
//! somewhere to get the picture from — Steam publishes a capsule, a
//! PlayStation 3 game carries `ICON0.PNG`, a ROM has box art somebody
//! fetched — and for a long time the Windows column had nothing, so every
//! game in it wore the same mark and a shelf of six programs was six copies
//! of one drawing.
//!
//! It had nothing because the question was asked about the wrong thing.
//! Nobody publishes cover art for a folder somebody installed, which is
//! true; but a Windows program has carried its own drawing since Windows
//! 3.0, it is the drawing its own author chose, it is the one the person
//! recognises from every other machine they have used it on, and it is
//! sitting inside the file the shell already opens to find out what the
//! program is called.
//!
//! ## What this does
//!
//! Reads `RT_GROUP_ICON` — the little table that says which of a program's
//! drawings is which size — picks the one worth drawing at the size a row
//! is, reads that `RT_ICON`, and writes it out as a PNG beside the game's
//! `lxb.json`. Once: [`beside`] answers with the file it wrote last time
//! rather than reading the executable again, so a column of twenty games
//! costs twenty stats.
//!
//! ## Two shapes come out of a `.exe`
//!
//! A modern program stores its largest drawing as a PNG, which is handed
//! on untouched. Everything else is a Windows DIB: the same bitmap a `.bmp`
//! holds, bottom row first, with a second one-bit image under it that says
//! which pixels are see-through. Both are decoded here, because a program
//! that only has the old kind is exactly the old program whose icon a
//! person is most likely to recognise.
//!
//! ## This file is not ours
//!
//! The executable is one somebody picked off their disk. Read on the terms
//! [`crate::pe`] sets out: every offset checked, every count capped,
//! nothing trusted because a header said it, and `None` rather than a
//! failure for anything that does not add up.

use std::path::{Path, PathBuf};

/// What the picture beside a game is called.
const FILE: &str = "icon.png";

/// Bigger than this is not worth keeping. A row draws the picture at
/// something under a hundred pixels and the shell's own thumbnailer scales
/// whatever it is given, so the 256-pixel drawing a modern program carries
/// is already more than enough — and this bounds a file claiming its icon
/// is forty thousand pixels square.
const BIGGEST: u32 = 256;

/// How many drawings one icon group may offer. A real program has between
/// three and ten.
const MOST_IMAGES: usize = 64;

/// The picture for a game, written beside it, or `None` where the program
/// carries no drawing.
///
/// `exe` is what the game plays and `game_dir` is the game's own directory
/// — the picture goes in there and not in a cache, so it is removed when
/// the game is, and so a games folder somebody moves to another disk takes
/// its pictures with it.
///
/// Answers the file it wrote before without opening the executable again.
/// Nothing re-reads a program that had no icon the first time, either: that
/// costs one failed read per listing, which is one bounded read of a file
/// already on this disk, and the alternative is a marker file saying
/// nothing.
pub fn beside(game_dir: &Path, exe: &Path) -> Option<PathBuf> {
    let at = game_dir.join(FILE);
    if at.is_file() {
        return Some(at);
    }
    let png = of(exe)?;
    // Beside itself and then over itself, as `lxb.json` is written, so a
    // listing that runs while this one is being written reads the whole of
    // the old file or the whole of the new one.
    let temp = game_dir.join(format!("{FILE}.lxb-{}", std::process::id()));
    std::fs::write(&temp, &png).ok()?;
    match std::fs::rename(&temp, &at) {
        Ok(()) => Some(at),
        Err(_) => {
            let _ = std::fs::remove_file(&temp);
            None
        }
    }
}

/// The same picture, written to a file the caller named, answering whether
/// there was one.
///
/// What [`beside`] is for a game, this is for a file nobody has installed:
/// one row of the shell's file explorer — a `.exe` sitting in somebody's
/// Downloads, a `.msi` on a memory stick — which has no folder of its own
/// for a picture to live in and may well be on a disk this user cannot
/// write to. So the caller names the file, and what it names is a scratch
/// of its own that it reads and drops. Nothing is kept here and nothing is
/// cached: the row is asked about once, and after that the picture is in the
/// shell's own atlas.
///
/// Written straight, where [`beside`] writes beside and then over itself.
/// That one lands in a directory a listing walks, so a reader must never
/// meet half a file; this one goes somewhere the caller made for itself and
/// nothing else looks at.
pub fn written_to(exe: &Path, at: &Path) -> bool {
    let Some(png) = of(exe) else {
        return false;
    };
    std::fs::write(at, &png).is_ok()
}

/// The picture inside one executable, as the bytes of a PNG.
pub fn of(exe: &Path) -> Option<Vec<u8>> {
    let read = crate::pe::Reader::of(exe)?;
    let group = read.resource(crate::pe::RT_GROUP_ICON, None)?;
    let name = best_of(group)?;
    let image = read.resource(crate::pe::RT_ICON, Some(name))?;
    // A program built this decade stores its largest drawing as a PNG
    // already, and the best thing to do with it is nothing.
    if image.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return (image.len() <= 4 * 1024 * 1024).then(|| image.to_vec());
    }
    let (width, height, rgba) = dib(image)?;
    encode(width, height, &rgba)
}

/// Which of a group's drawings to take, by the name it is filed under.
///
/// `GRPICONDIR` is six bytes of header and then fourteen per drawing. The
/// biggest is wanted, and the deepest where two are the same size — which
/// is how a program that carries both a 32-colour and a true-colour 32×32
/// gets the one that still looks like something.
fn best_of(group: &[u8]) -> Option<u32> {
    let count = usize::from(u16_at(group, 4)?).min(MOST_IMAGES);
    let mut best: Option<(u32, u16, u32)> = None;
    for index in 0..count {
        // A header may claim more drawings than the file carries. What has
        // been read so far is still the answer — stopping is right, and
        // throwing away the drawing already found would be a program with
        // an icon drawn as one without.
        let Some(entry) = 6usize
            .checked_add(index.checked_mul(14)?)
            .and_then(|at| group.get(at..at.checked_add(14)?))
        else {
            break;
        };
        // Zero means 256 in a field one byte wide, which is how the format
        // was made to hold a drawing larger than it was designed for.
        let width = match entry[0] {
            0 => 256u32,
            byte => u32::from(byte),
        };
        let height = match entry[1] {
            0 => 256u32,
            byte => u32::from(byte),
        };
        let depth = u16_at(entry, 6)?;
        let name = u32::from(u16_at(entry, 12)?);
        if width > BIGGEST || height > BIGGEST {
            continue;
        }
        let size = width.saturating_mul(height);
        if best.is_none_or(|(had, had_depth, _)| (size, depth) > (had, had_depth)) {
            best = Some((size, depth, name));
        }
    }
    best.map(|(_, _, name)| name)
}

/// One Windows bitmap out of an executable, as width, height and RGBA.
///
/// The shape is `BITMAPINFOHEADER`, then the colour table where the depth
/// has one, then the image bottom row first, then a one-bit mask of the
/// same shape saying which pixels are see-through. The header's height
/// counts both images, which is why it is halved.
fn dib(image: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let header = u32_at(image, 0)?;
    if header < 40 {
        return None;
    }
    let width = u32_at(image, 4)?;
    let doubled = u32_at(image, 8)?;
    let depth = u16_at(image, 14)?;
    let compression = u32_at(image, 16)?;
    // Only the uncompressed kind. An icon is never anything else, and a
    // header saying otherwise is a file to walk away from rather than a
    // decoder to write.
    if compression != 0 {
        return None;
    }
    let height = doubled.checked_div(2)?;
    if width == 0 || height == 0 || width > BIGGEST || height > BIGGEST {
        return None;
    }

    // How many colours the table holds, where the depth has one. A header
    // may say, and where it says nothing the depth decides.
    let palette_length = match depth {
        1 | 4 | 8 => match u32_at(image, 32)? {
            0 => 1usize.checked_shl(u32::from(depth))?,
            said => (said as usize).min(256),
        },
        _ => 0,
    };
    let palette_at = header as usize;
    let pixels_at = palette_at.checked_add(palette_length.checked_mul(4)?)?;
    let palette = image.get(palette_at..pixels_at).unwrap_or(&[]);

    // Rows are padded to four bytes, in both images.
    let row = |bits: u32| -> Option<usize> {
        let bits = width.checked_mul(bits)?;
        Some((bits.checked_add(31)? / 32) as usize * 4)
    };
    let colour_row = row(u32::from(depth))?;
    let mask_row = row(1)?;
    let colours_end = pixels_at.checked_add(colour_row.checked_mul(height as usize)?)?;
    if colours_end > image.len() {
        return None;
    }
    // The mask may be missing from a 32-bit drawing, whose own alpha says
    // everything the mask would have. Where it is absent nothing is
    // see-through, which is what an icon with no mask means.
    let mask =
        image.get(colours_end..colours_end.checked_add(mask_row.checked_mul(height as usize)?)?);

    let mut rgba = vec![
        0u8;
        (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?
    ];
    let mut any_alpha = false;
    for y in 0..height as usize {
        // Bottom row first: the last row of the file is the top of the
        // picture.
        let source = (height as usize - 1 - y).checked_mul(colour_row)?;
        for x in 0..width as usize {
            let (r, g, b, mut a) = match depth {
                32 => {
                    let at = pixels_at
                        .checked_add(source)?
                        .checked_add(x.checked_mul(4)?)?;
                    let four = image.get(at..at.checked_add(4)?)?;
                    (four[2], four[1], four[0], four[3])
                }
                24 => {
                    let at = pixels_at
                        .checked_add(source)?
                        .checked_add(x.checked_mul(3)?)?;
                    let three = image.get(at..at.checked_add(3)?)?;
                    (three[2], three[1], three[0], 255)
                }
                8 | 4 | 1 => {
                    let index = indexed(image, pixels_at.checked_add(source)?, x, depth)?;
                    let at = usize::from(index).checked_mul(4)?;
                    let four = palette.get(at..at.checked_add(4)?)?;
                    (four[2], four[1], four[0], 255)
                }
                _ => return None,
            };
            if depth == 32 {
                any_alpha |= a != 0;
            }
            // The mask is the older way of saying see-through, and it is
            // the only one the shallow depths have. A set bit means the
            // pixel shows what is behind it.
            if let Some(mask) = mask {
                let row_at = (height as usize - 1 - y).checked_mul(mask_row)?;
                let byte = *mask.get(row_at.checked_add(x / 8)?)?;
                if byte & (0x80 >> (x % 8)) != 0 {
                    a = 0;
                }
            }
            let out = y
                .checked_mul(width as usize)?
                .checked_add(x)?
                .checked_mul(4)?;
            let pixel = rgba.get_mut(out..out.checked_add(4)?)?;
            pixel.copy_from_slice(&[r, g, b, a]);
        }
    }
    // A 32-bit drawing whose alpha is zero the whole way through is one
    // written by a program that never filled it in — Windows draws those
    // by the mask alone, and a picture this shell drew from that alpha
    // would be a hole in the row.
    if depth == 32 && !any_alpha {
        for pixel in rgba.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        if let Some(mask) = mask {
            for y in 0..height as usize {
                let row_at = (height as usize - 1 - y) * mask_row;
                for x in 0..width as usize {
                    let Some(byte) = mask.get(row_at + x / 8) else {
                        continue;
                    };
                    if byte & (0x80 >> (x % 8)) != 0 {
                        if let Some(pixel) = rgba.get_mut((y * width as usize + x) * 4 + 3) {
                            *pixel = 0;
                        }
                    }
                }
            }
        }
    }
    Some((width, height, rgba))
}

/// One pixel out of a row of 1-, 4- or 8-bit indices.
fn indexed(image: &[u8], row_at: usize, x: usize, depth: u16) -> Option<u8> {
    match depth {
        8 => image.get(row_at.checked_add(x)?).copied(),
        4 => {
            let byte = *image.get(row_at.checked_add(x / 2)?)?;
            Some(match x % 2 {
                0 => byte >> 4,
                _ => byte & 0x0f,
            })
        }
        1 => {
            let byte = *image.get(row_at.checked_add(x / 8)?)?;
            Some((byte >> (7 - (x % 8))) & 1)
        }
        _ => None,
    }
}

/// Write RGBA out as a PNG.
fn encode(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(rgba).ok()?;
    writer.finish().ok()?;
    Some(out)
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let pair = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes([pair[0], pair[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let four = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([four[0], four[1], four[2], four[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `GRPICONDIRENTRY`: width, height, colours, reserved, planes,
    /// depth, bytes, and the name the image is filed under.
    fn entry(width: u8, height: u8, depth: u16, name: u16) -> Vec<u8> {
        let mut out = vec![width, height, 0, 0];
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&depth.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&name.to_le_bytes());
        out
    }

    fn group(entries: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0, 0, 1, 0];
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        for one in entries {
            out.extend_from_slice(one);
        }
        out
    }

    /// The biggest drawing, and the deepest of two the same size — which is
    /// the whole of what a group is read for.
    #[test]
    fn the_largest_and_deepest_drawing_is_the_one_taken() {
        let found = best_of(&group(&[
            entry(16, 16, 32, 1),
            entry(48, 48, 32, 2),
            entry(32, 32, 32, 3),
        ]));
        assert_eq!(found, Some(2), "48 beats 32 and 16");

        let found = best_of(&group(&[entry(32, 32, 4, 7), entry(32, 32, 32, 8)]));
        assert_eq!(found, Some(8), "the same size, so the deeper one");

        // Zero is how a byte-wide field says 256.
        let found = best_of(&group(&[entry(48, 48, 32, 4), entry(0, 0, 32, 5)]));
        assert_eq!(found, Some(5), "0 means 256, which is the biggest");
    }

    /// Nothing out of a group that says nothing, and nothing out of one
    /// that claims more drawings than it carries.
    #[test]
    fn a_group_that_does_not_add_up_offers_nothing() {
        assert_eq!(best_of(&[]), None);
        assert_eq!(best_of(&group(&[])), None);
        let mut lying = group(&[entry(32, 32, 32, 1)]);
        lying[4..6].copy_from_slice(&9999u16.to_le_bytes());
        // Nine thousand claimed, one carried: the one is still found and
        // nothing past the end is read.
        assert_eq!(best_of(&lying), Some(1));
    }

    /// A drawing larger than anything a row draws is passed over rather
    /// than decoded: 40,000 pixels square is sixteen gigabytes.
    #[test]
    fn a_drawing_too_big_to_be_one_is_not_taken() {
        // Both entries are written as 256 by the zero convention, so the
        // guard inside `dib` is what the oversized header meets.
        let huge = {
            let mut header = vec![0u8; 40];
            header[0..4].copy_from_slice(&40u32.to_le_bytes());
            header[4..8].copy_from_slice(&40_000u32.to_le_bytes());
            header[8..12].copy_from_slice(&80_000u32.to_le_bytes());
            header[14..16].copy_from_slice(&32u16.to_le_bytes());
            header
        };
        assert_eq!(dib(&huge), None);
    }

    /// A 32-bit drawing, which is what nearly every program carries: four
    /// bytes a pixel, bottom row first, blue before red.
    #[test]
    fn a_true_colour_drawing_comes_out_the_right_way_up() {
        let (width, height) = (2u32, 2u32);
        let mut image = vec![0u8; 40];
        image[0..4].copy_from_slice(&40u32.to_le_bytes());
        image[4..8].copy_from_slice(&width.to_le_bytes());
        image[8..12].copy_from_slice(&(height * 2).to_le_bytes());
        image[12..14].copy_from_slice(&1u16.to_le_bytes());
        image[14..16].copy_from_slice(&32u16.to_le_bytes());
        // The bottom row of the file is the bottom of the picture: two
        // blues, then two reds above them.
        for (b, g, r) in [(255, 0, 0), (255, 0, 0), (0, 0, 255), (0, 0, 255)] {
            image.extend_from_slice(&[b, g, r, 255]);
        }
        image.extend_from_slice(&[0u8; 8]); // the mask, nothing set

        let (w, h, rgba) = dib(&image).expect("a drawing this shape is read");
        assert_eq!((w, h), (2, 2));
        assert_eq!(&rgba[0..4], &[255, 0, 0, 255], "red is the top row");
        assert_eq!(&rgba[12..16], &[0, 0, 255, 255], "blue is the bottom");

        // And it encodes to something a PNG reader will take.
        let png = encode(w, h, &rgba).expect("it encodes");
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
    }

    /// The mask is what says see-through on every drawing that has one.
    #[test]
    fn the_mask_is_what_makes_a_pixel_see_through() {
        let mut image = vec![0u8; 40];
        image[0..4].copy_from_slice(&40u32.to_le_bytes());
        image[4..8].copy_from_slice(&1u32.to_le_bytes());
        image[8..12].copy_from_slice(&2u32.to_le_bytes());
        image[14..16].copy_from_slice(&24u16.to_le_bytes());
        // One pixel, 24-bit, padded to four bytes.
        image.extend_from_slice(&[7, 8, 9, 0]);
        // One mask row, its first bit set: that pixel shows what is behind.
        image.extend_from_slice(&[0x80, 0, 0, 0]);

        let (_, _, rgba) = dib(&image).expect("a drawing this shape is read");
        assert_eq!(rgba, vec![9, 8, 7, 0], "the mask took the pixel out");
    }

    /// A file that is not an executable has no drawing in it, and nothing
    /// is written beside a game for one.
    #[test]
    fn nothing_is_written_for_a_file_with_no_drawing_in_it() {
        let home = tempfile::tempdir().unwrap();
        let exe = home.path().join("notes.txt");
        std::fs::write(&exe, b"this is not a program").unwrap();
        assert_eq!(of(&exe), None);
        assert_eq!(beside(home.path(), &exe), None);
        assert!(!home.path().join(FILE).exists(), "and no empty file either");
    }

    /// Written once. The second question is answered by the file the first
    /// one wrote, which is what makes this cheap enough to ask on every
    /// listing.
    #[test]
    fn the_picture_is_written_once_and_then_read_back() {
        let home = tempfile::tempdir().unwrap();
        let exe = home.path().join("not-even-opened.exe");
        std::fs::write(home.path().join(FILE), b"a picture from before").unwrap();
        let found = beside(home.path(), &exe).expect("the one already there");
        assert_eq!(found, home.path().join(FILE));
        assert_eq!(
            std::fs::read(&found).unwrap(),
            b"a picture from before",
            "it was not written over"
        );
    }

    /// A file with no drawing in it leaves nothing where the shell asked
    /// for one, so a reader that finds a file there knows there was a
    /// drawing — which is the whole of how the two halves tell the two
    /// answers apart.
    #[test]
    fn nothing_is_left_where_a_file_without_a_drawing_was_asked_about() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("run.bat");
        std::fs::write(&exe, b"@echo off\r\nstart game.exe\r\n").unwrap();
        let at = dir.path().join("asked-for.png");

        assert!(!written_to(&exe, &at), "a script carries no drawing");
        assert!(!at.exists(), "and no empty file was left behind");
    }

    /// Read a real executable's drawing, when one is named — opt-in for the
    /// reason [`crate::pe`]'s own does: nothing here may depend on a file
    /// on one developer's disk.
    ///
    /// ```text
    /// LXB_PROTON_TEST_EXE=/path/to/a.exe cargo test -p lxb-proton -- --nocapture
    /// ```
    #[test]
    fn a_real_executable_carries_its_own_drawing() {
        let Some(at) = std::env::var_os("LXB_PROTON_TEST_EXE") else {
            return;
        };
        let at = std::path::PathBuf::from(at);
        match of(&at) {
            Some(png) => println!("{} -> {} bytes of PNG", at.display(), png.len()),
            None => println!("{} -> no drawing", at.display()),
        }

        // And the shape the shell asks in: the drawing lands at the name the
        // caller chose, as a PNG and nothing else. Beside `of` rather than in
        // a test of its own, because without a real executable there is
        // nothing for it to write — and a test that wrote the PNG itself
        // before asking would be a test of `std::fs::write`.
        let dir = tempfile::tempdir().unwrap();
        let scratch = dir.path().join("asked-for.png");
        if written_to(&at, &scratch) {
            let written = std::fs::read(&scratch).expect("the file it said it wrote");
            assert_eq!(
                &written[..8],
                &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a],
                "a reader of that file gets a PNG"
            );
            assert_eq!(Some(written), of(&at), "and the whole of what was read");
        } else {
            assert!(!scratch.exists(), "nothing is left behind for no drawing");
        }
    }
}
