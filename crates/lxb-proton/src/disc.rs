//! A disc in the drive, read the way Windows reads one when it goes in.
//!
//! Windows opens a disc on its own: it looks at `autorun.inf` at the top of
//! it and runs whatever `Open=` names, wearing whatever `Icon=` names, under
//! whatever `Label=` says. A disc made for Windows was made to be met that
//! way, so this reads the same three lines — and where a disc has no
//! `autorun.inf`, takes the program at its top that is plainly the way in:
//! one called `setup`, `install` or `autorun`, or the only one there is.
//!
//! **Nothing deeper than the top is guessed at.** A disc with programs only
//! in its folders is a disc of somebody's files, or a game that never meant to
//! be started from the drive, and a row standing in front of one of its
//! programs would be this shell choosing for them. That is the user's own
//! line, drawn on 2026-10-10.
//!
//! ## What a disc looks like from here
//!
//! Mounted, by the time anything here sees it — the shell's drive worker
//! mounts what UDisks says may be. So this reads files, not sectors. What that
//! leaves to take care of is how a disc names them:
//!
//! - `autorun.inf` is a Windows text file: CRLF, a code page rather than
//!   UTF-8, sections and keys in whatever case its author typed, and
//!   comments after `;`.
//! - A name in it is a Windows path: backslashes, any case, and an `.exe` it
//!   may leave off. The disc's own names are in whatever case the disc was
//!   pressed with, and an ISO 9660 disc without Joliet is all capitals. So
//!   every name is looked up by walking the real directory and comparing
//!   without case — see [`find`].
//! - `Open=` is a command line, with arguments after the program and quotes
//!   round a program whose name has a space in it.
//!
//! ## This disc is not ours
//!
//! It is whatever somebody put in the drive. Read on the terms `pe` sets out:
//! every size capped, nothing followed out of the disc, and `None` rather than
//! a failure for anything that does not add up.

use std::path::{Component, Path, PathBuf};

use crate::prefix::Game;
use crate::report::Disc;

/// The endings Windows runs as a program — the four **Run a Windows program**
/// offers. What `Open=` names must be one of them: the key is for programs,
/// and `ShellExecute=` is the one that may name a document.
const RUNS: &[&str] = &["exe", "msi", "bat", "cmd"];

/// What a disc's way in is called when it has no `autorun.inf` to say so, in
/// the order they are believed. An installer is what a disc made for Windows
/// most often opens with; `autorun.exe` is what a menu that ran from the
/// drive was usually called.
const WAYS_IN: &[&str] = &["setup", "install", "autorun"];

/// More than any real `autorun.inf`, and the most of one this reads.
const LARGEST_AUTORUN: u64 = 64 * 1024;

/// What a disc's `autorun.inf` says, as far as this shell listens.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Autorun {
    /// The program Windows runs when the disc goes in, with whatever comes
    /// after it.
    pub open: Option<String>,
    /// The same, for a disc that names something to be opened rather than
    /// run. Read only where it names a program.
    pub shell_execute: Option<String>,
    /// The picture Windows shows for the drive: an `.ico`, or a program with
    /// an icon in it, and perhaps `,N` after it.
    pub icon: Option<String>,
    /// What Windows calls the drive while the disc is in it.
    pub label: Option<String>,
}

/// Read the `[autorun]` section of an `autorun.inf`.
///
/// A 64-bit Windows reads `[autorun.amd64]` first and `[autorun]` for what
/// that one does not say, and a Proton prefix is a 64-bit Windows — so this
/// does the same. Every other section is somebody else's.
pub fn autorun(text: &str) -> Autorun {
    let mut plain = Autorun::default();
    let mut amd64 = Autorun::default();
    let mut section: Option<&mut Autorun> = None;
    for line in text.lines() {
        let line = without_a_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            section = match name.trim().to_ascii_lowercase().as_str() {
                "autorun" => Some(&mut plain),
                "autorun.amd64" => Some(&mut amd64),
                _ => None,
            };
            continue;
        }
        let Some(said) = section.as_deref_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let slot = match key.trim().to_ascii_lowercase().as_str() {
            "open" => &mut said.open,
            "shellexecute" => &mut said.shell_execute,
            "icon" => &mut said.icon,
            "label" => &mut said.label,
            _ => continue,
        };
        // The first of two is the one Windows keeps.
        if slot.is_none() {
            *slot = Some(value.to_string());
        }
    }
    Autorun {
        open: amd64.open.or(plain.open),
        shell_execute: amd64.shell_execute.or(plain.shell_execute),
        icon: amd64.icon.or(plain.icon),
        label: amd64.label.or(plain.label),
    }
}

/// A line of an `.inf` with its comment taken off: everything from the first
/// `;` that is not inside quotes.
fn without_a_comment(line: &str) -> &str {
    let mut quoted = false;
    for (at, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ';' if !quoted => return &line[..at],
            _ => {}
        }
    }
    line
}

/// The text of a file written on Windows, whatever it was written in.
///
/// UTF-16 where it says so with its first two bytes, UTF-8 where it is that,
/// and otherwise one character a byte — which is what a Western code page is
/// for every character a program's name is made of, and only gets an accent
/// wrong in a name somebody else's code page wrote.
fn text_of(bytes: &[u8]) -> String {
    if let Some(wide) = bytes.strip_prefix(&[0xff, 0xfe]) {
        let units: Vec<u16> = wide
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => bytes.iter().map(|&byte| char::from(byte)).collect(),
    }
}

/// A file on the disc, by a Windows path relative to its top, or `None`
/// where there is none.
///
/// Every step is matched against what is really in the directory without
/// regard to case, because the path was typed on Windows and the names were
/// pressed onto the disc by whatever made it. A step that would leave the
/// disc — `..`, a drive letter, or a link, which a Rock Ridge or UDF disc can
/// carry and which can point anywhere — finds nothing: what `autorun.inf`
/// names is on the disc it is on.
pub fn find(root: &Path, windows: &str) -> Option<PathBuf> {
    let written = windows.trim().replace('\\', "/");
    let mut at = root.to_path_buf();
    for step in Path::new(written.trim_start_matches('/')).components() {
        let name = match step {
            Component::Normal(name) => name.to_str()?,
            Component::CurDir => continue,
            _ => return None,
        };
        // A drive letter is somewhere else; `autorun.inf` naming the disc's
        // own (`D:\setup.exe`) is a disc pressed for one drive.
        if name.len() == 2 && name.ends_with(':') {
            return None;
        }
        at = match at.join(name) {
            exact if exact.exists() => exact,
            _ => std::fs::read_dir(&at)
                .ok()?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|entry| {
                    entry
                        .file_name()
                        .and_then(|found| found.to_str())
                        .is_some_and(|found| found.eq_ignore_ascii_case(name))
                })?,
        };
        if std::fs::symlink_metadata(&at)
            .ok()?
            .file_type()
            .is_symlink()
        {
            return None;
        }
    }
    (at != root).then_some(at)
}

/// Whether Windows runs this file as a program.
fn runs(at: &Path) -> bool {
    at.extension()
        .and_then(|end| end.to_str())
        .is_some_and(|end| RUNS.iter().any(|runs| end.eq_ignore_ascii_case(runs)))
}

/// The program a command line on the disc runs, and what follows it.
///
/// The way Windows reads one: a program in quotes is the quoted part, and an
/// unquoted one is the shortest run of words that names a file — so
/// `My Game.exe /play` is `My Game.exe` and `/play`, and `setup /s` is
/// `setup.exe` and `/s`, since a program may be named without its ending.
pub fn command(root: &Path, line: &str) -> Option<(PathBuf, Vec<String>)> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix('"') {
        let (program, after) = rest.split_once('"').unwrap_or((rest, ""));
        let at = a_program(root, program)?;
        return Some((at, words(after)));
    }
    // Every place a run of words could end, shortest first — and what is
    // after it read as a command line's words are, quotes and all.
    line.char_indices()
        .filter(|(_, c)| c.is_whitespace())
        .map(|(at, _)| at)
        .chain(std::iter::once(line.len()))
        .find_map(|end| {
            let at = a_program(root, &line[..end])?;
            Some((at, words(&line[end..])))
        })
}

/// A program on the disc by the name a command line gives it, with `.exe`
/// put on where it was left off.
fn a_program(root: &Path, name: &str) -> Option<PathBuf> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    find(root, name)
        .filter(|at| at.is_file())
        .or_else(|| find(root, &format!("{name}.exe")).filter(|at| at.is_file()))
        .filter(|at| runs(at))
}

/// What follows a program on a command line, a word at a time, with quotes
/// holding a word with spaces in it together.
fn words(line: &str) -> Vec<String> {
    let mut said = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    said.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        said.push(word);
    }
    said
}

/// The program a press on this disc runs, and what to put after it.
///
/// `autorun.inf` first, because a disc that has one has said: `Open=`, then
/// `ShellExecute=` where it names a program. Then the top of the disc, for a
/// disc that has none — see [`WAYS_IN`] for the names believed, or the one
/// program there is when there is only one. Nothing further down.
pub fn program(root: &Path, said: &Autorun) -> Option<(PathBuf, Vec<String>)> {
    if let Some(found) = [&said.open, &said.shell_execute]
        .into_iter()
        .flatten()
        .find_map(|line| command(root, line))
    {
        return Some(found);
    }
    let mut programs: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|at| {
            at.is_file()
                && at
                    .extension()
                    .and_then(|end| end.to_str())
                    .is_some_and(|end| end.eq_ignore_ascii_case("exe"))
        })
        .collect();
    programs.sort();
    let called = |at: &PathBuf, name: &str| {
        at.file_stem()
            .and_then(|stem| stem.to_str())
            .is_some_and(|stem| stem.eq_ignore_ascii_case(name))
    };
    if let Some(way_in) = WAYS_IN
        .iter()
        .find_map(|name| programs.iter().find(|at| called(at, name)))
    {
        return Some((way_in.clone(), Vec::new()));
    }
    match programs.as_slice() {
        [only] => Some((only.clone(), Vec::new())),
        _ => None,
    }
}

/// What the disc is called on the bar.
///
/// `Label=` first, because it is what the disc's author wanted Windows to
/// call the drive. Then the label pressed onto the disc itself — on a disc
/// without Joliet that is upper case with underscores for spaces, and the
/// underscores are given back as spaces, but the capitals are left: a disc
/// called `FIFA_2002` is not called *Fifa 2002*. Then what the program calls
/// itself, which on a disc whose way in is a menu is often nothing at all.
/// Then the program's own file name, which is always something.
pub fn name(said: &Autorun, label: Option<&str>, program: &Path) -> String {
    let given = |name: &str| {
        let name = name.trim().trim_matches('"').trim();
        (!name.is_empty()).then(|| name.to_string())
    };
    said.label
        .as_deref()
        .and_then(given)
        .or_else(|| {
            label
                .and_then(given)
                .map(|label| match label.contains(' ') {
                    true => label,
                    false => label.replace('_', " "),
                })
        })
        .or_else(|| crate::pe::product_name(program))
        .or_else(|| {
            program
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "Windows disc".to_string())
}

/// The disc's own picture, as a PNG: `Icon=` where it names something with a
/// drawing in it, and the program's own drawing otherwise.
///
/// `Icon=` may be an `.ico` or a program, and may have `,N` after it to say
/// which of a program's drawings — which is taken as the program and its
/// best drawing, the way a game's row already wears one.
pub fn picture(root: &Path, said: &Autorun, program: &Path) -> Option<Vec<u8>> {
    let named = said.icon.as_deref().and_then(|icon| {
        // The `,N` first and the quotes after it: `"Disc.ico",0` is quoted
        // round the file, not round the whole of it.
        let icon = icon.trim();
        let file = match icon.rsplit_once(',') {
            Some((file, index)) if index.trim().parse::<i32>().is_ok() => file,
            _ => icon,
        };
        // A file, and only a file: what is opened here is read to its end,
        // and a pipe on the disc would be read for ever.
        let at = find(root, file.trim().trim_matches('"'))?;
        crate::icon::of(&at.is_file().then_some(at)?)
    });
    named.or_else(|| crate::icon::of(program))
}

/// Everything a row needs from one disc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    pub name: String,
    pub png: Option<Vec<u8>>,
}

/// Read the disc mounted at `root`, whose file system is called `label`.
///
/// `None` for a disc with nothing on it for Windows to run.
pub fn read(root: &Path, label: Option<&str>) -> Option<Reading> {
    let said = find(root, "autorun.inf")
        .filter(|at| at.is_file())
        .and_then(|at| {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(at)
                .ok()?
                .take(LARGEST_AUTORUN)
                .read_to_end(&mut bytes)
                .ok()?;
            Some(autorun(&text_of(&bytes)))
        })
        .unwrap_or_default();
    let (program, arguments) = program(root, &said)?;
    Some(Reading {
        name: name(&said, label, &program),
        png: picture(root, &said, &program),
        program,
        arguments,
    })
}

/// The Windows a disc has already been given, where it has one that has not
/// installed anything — see [`Game::the_discs_own`].
///
/// Where somebody has managed two, the first: either is that disc's, and the
/// shell shows neither.
pub fn its_own_windows<'a>(games: &'a [Game], disc: &Disc) -> Option<&'a Game> {
    games.iter().find(|game| game.the_discs_own(disc))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disc's top, with these files on it, each holding a few bytes.
    fn a_disc(files: &[&str]) -> tempfile::TempDir {
        let disc = tempfile::tempdir().unwrap();
        for file in files {
            let at = disc.path().join(file);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(at, b"MZ").unwrap();
        }
        disc
    }

    /// **The three lines a disc made for Windows says itself with**, written
    /// the way such a file is: CRLF, keys in whatever case, a comment.
    #[test]
    fn autorun_says_what_to_run_what_it_wears_and_what_it_is_called() {
        let said = autorun(
            "; made by the publisher\r\n[AutoRun]\r\nOPEN=Setup.exe /autorun\r\n\
             icon=Disc.ico,0 ; the drive's picture\r\nLabel=\"Some Game\"\r\n",
        );
        assert_eq!(said.open.as_deref(), Some("Setup.exe /autorun"));
        assert_eq!(said.icon.as_deref(), Some("Disc.ico,0"));
        assert_eq!(said.label.as_deref(), Some("\"Some Game\""));
    }

    /// A 64-bit Windows reads its own section first and the plain one for
    /// the rest, and nobody else's section is read at all.
    #[test]
    fn the_sixty_four_bit_section_is_read_first() {
        let said = autorun(
            "[autorun.mips]\nopen=wrong.exe\n[autorun]\nopen=setup32.exe\nicon=disc.ico\n\
             [autorun.amd64]\nopen=setup64.exe\n[Strings]\nlabel=nobody's\n",
        );
        assert_eq!(said.open.as_deref(), Some("setup64.exe"));
        assert_eq!(said.icon.as_deref(), Some("disc.ico"), "from [autorun]");
        assert_eq!(said.label, None, "[Strings] is not this file's to give");
    }

    /// A file written in UTF-16, or in a code page, is still read.
    #[test]
    fn autorun_is_read_in_whatever_it_was_written_in() {
        let wide: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain(
                "[autorun]\r\nopen=start.exe\r\n"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes),
            )
            .collect();
        assert_eq!(autorun(&text_of(&wide)).open.as_deref(), Some("start.exe"));

        // `ó` in Windows-1252 is one byte, which is not UTF-8.
        let narrow = b"[autorun]\r\nlabel=Gr\xf3\r\n";
        assert_eq!(autorun(&text_of(narrow)).label.as_deref(), Some("Gró"));
    }

    /// **A name in `autorun.inf` is a Windows path**, and the disc's own
    /// names are in whatever case it was pressed with.
    #[test]
    fn a_windows_path_finds_the_file_whatever_its_case() {
        let disc = a_disc(&["SETUP/Install.EXE", "readme.txt"]);
        let root = disc.path();
        assert_eq!(
            find(root, r"setup\install.exe"),
            Some(root.join("SETUP/Install.EXE"))
        );
        assert_eq!(
            find(root, r".\SETUP\INSTALL.EXE"),
            Some(root.join("SETUP/Install.EXE"))
        );
        assert_eq!(find(root, r"\readme.txt"), Some(root.join("readme.txt")));
        assert_eq!(find(root, "missing.exe"), None);
    }

    /// Nothing `autorun.inf` names is off the disc it is on.
    #[test]
    fn a_path_never_leaves_the_disc() {
        let disc = a_disc(&["setup.exe"]);
        let inside = disc.path().join("inner");
        std::fs::create_dir(&inside).unwrap();
        assert_eq!(find(&inside, r"..\setup.exe"), None);
        assert_eq!(find(disc.path(), r"D:\setup.exe"), None);
        assert_eq!(find(disc.path(), ""), None, "the disc itself is not a file");
    }

    /// `Open=` is a command line: the program, and what follows it.
    #[test]
    fn open_is_a_program_and_what_follows_it() {
        let disc = a_disc(&["Setup.exe", "My Game/Play Now.exe"]);
        let root = disc.path();
        assert_eq!(
            command(root, "setup /autorun lang=en"),
            Some((
                root.join("Setup.exe"),
                vec!["/autorun".into(), "lang=en".into()]
            )),
            "the ending may be left off, as Windows allows"
        );
        assert_eq!(
            command(root, r#""My Game\Play Now.exe" -window "two words""#),
            Some((
                root.join("My Game/Play Now.exe"),
                vec!["-window".into(), "two words".into()]
            ))
        );
        assert_eq!(
            command(root, r"my game\play now.exe /x"),
            Some((root.join("My Game/Play Now.exe"), vec!["/x".into()])),
            "unquoted, the shortest run of words that names a file"
        );
        assert_eq!(command(root, "readme.txt"), None);
        assert_eq!(
            command(root, r#"setup.exe /title "Two Words"  /x"#),
            Some((
                root.join("Setup.exe"),
                vec!["/title".into(), "Two Words".into(), "/x".into()]
            )),
            "an unquoted program's arguments are words the way a quoted one's are"
        );
    }

    /// A link on the disc is not followed: it can point anywhere at all.
    #[test]
    fn a_link_on_the_disc_is_not_followed() {
        let elsewhere = a_disc(&["secret.exe"]);
        let disc = a_disc(&["setup.exe"]);
        std::os::unix::fs::symlink(
            elsewhere.path().join("secret.exe"),
            disc.path().join("away.exe"),
        )
        .unwrap();
        assert_eq!(find(disc.path(), "away.exe"), None);
        assert_eq!(command(disc.path(), "away.exe"), None);
    }

    /// `Open=` names a program; a document it names is not one.
    #[test]
    fn open_must_name_something_windows_runs() {
        let disc = a_disc(&["index.html", "setup.exe"]);
        let said = Autorun {
            open: Some("index.html".into()),
            ..Autorun::default()
        };
        assert_eq!(
            program(disc.path(), &said),
            Some((disc.path().join("setup.exe"), Vec::new())),
            "the disc's top is read instead"
        );
    }

    /// **Without `autorun.inf`, the way in is the one plainly called that**,
    /// or the only program there is — and a disc with programs only further
    /// down has none.
    #[test]
    fn without_autorun_the_way_in_is_the_obvious_one_or_none() {
        let none = Autorun::default();

        let disc = a_disc(&["Game.exe", "SETUP.EXE", "Autorun.exe"]);
        assert_eq!(
            program(disc.path(), &none).map(|(at, _)| at),
            Some(disc.path().join("SETUP.EXE")),
            "setup before autorun, whatever the case"
        );

        let disc = a_disc(&["Tool.exe"]);
        assert_eq!(
            program(disc.path(), &none).map(|(at, _)| at),
            Some(disc.path().join("Tool.exe")),
            "the only program on the top of the disc"
        );

        let disc = a_disc(&["one.exe", "two.exe"]);
        assert_eq!(program(disc.path(), &none), None, "two, and neither said");

        let disc = a_disc(&["Apps/setup.exe", "photo.jpg"]);
        assert_eq!(program(disc.path(), &none), None, "nothing at the top");
    }

    /// The disc's own name before anything a program says, with a pressed
    /// label's underscores given back as spaces and its capitals kept.
    #[test]
    fn a_disc_is_called_what_it_says_it_is() {
        let disc = a_disc(&["start.exe"]);
        let program = disc.path().join("start.exe");
        let said = Autorun {
            label: Some("\"The Game\"".into()),
            ..Autorun::default()
        };
        assert_eq!(name(&said, Some("GAME_DISC_1"), &program), "The Game");

        let none = Autorun::default();
        assert_eq!(name(&none, Some("Some Game 2"), &program), "Some Game 2");
        assert_eq!(name(&none, Some("FIFA_2002"), &program), "FIFA 2002");
        assert_eq!(name(&none, Some("  "), &program), "start", "then the file");
        assert_eq!(name(&none, None, &program), "start");
    }

    /// `Icon=` with a `,N` after it is the file before the comma.
    #[test]
    fn an_icon_with_an_index_is_the_file_it_names() {
        let disc = a_disc(&["start.exe"]);
        // A two-by-two `.ico`, built by hand: one 32-bit drawing.
        let mut image = vec![0u8; 40];
        image[0..4].copy_from_slice(&40u32.to_le_bytes());
        image[4..8].copy_from_slice(&2u32.to_le_bytes());
        image[8..12].copy_from_slice(&4u32.to_le_bytes());
        image[12..14].copy_from_slice(&1u16.to_le_bytes());
        image[14..16].copy_from_slice(&32u16.to_le_bytes());
        image.extend_from_slice(&[9u8; 16]);
        image.extend_from_slice(&[0u8; 8]);
        let mut ico = vec![0, 0, 1, 0, 1, 0, 2, 2, 0, 0, 1, 0, 32, 0];
        ico.extend_from_slice(&(image.len() as u32).to_le_bytes());
        ico.extend_from_slice(&22u32.to_le_bytes());
        ico.extend_from_slice(&image);
        std::fs::write(disc.path().join("Disc.ICO"), &ico).unwrap();

        for written in ["disc.ico,0", "\"Disc.ico\",0", "\"DISC.ICO\""] {
            let said = Autorun {
                icon: Some(written.into()),
                ..Autorun::default()
            };
            let png = picture(disc.path(), &said, &disc.path().join("start.exe"));
            assert!(
                png.is_some_and(|png| png.starts_with(&[0x89, b'P', b'N', b'G'])),
                "{written}"
            );
        }

        // And a disc that names nothing and whose program carries nothing
        // has no picture, rather than a wrong one.
        let none = Autorun::default();
        assert_eq!(
            picture(disc.path(), &none, &disc.path().join("start.exe")),
            None
        );
    }

    /// **The whole of one disc**, shaped the way a compilation disc from a
    /// shop is: a menu at the top that `autorun.inf` names, the setups it
    /// offers in a folder, and no name of its own but the disc's.
    #[test]
    fn a_disc_with_a_menu_is_read_as_its_menu() {
        let disc = a_disc(&[
            "start.exe",
            "Apps/Game_setup.exe",
            "Apps/DirectX/DXSETUP.exe",
        ]);
        std::fs::write(
            disc.path().join("autorun.inf"),
            b"[autorun]\r\nIcon=missing.ico\r\nOpen=start.exe\r\n",
        )
        .unwrap();
        let read = read(disc.path(), Some("Some Compilation")).expect("a Windows disc");
        assert_eq!(read.program, disc.path().join("start.exe"));
        assert!(read.arguments.is_empty());
        assert_eq!(read.name, "Some Compilation");
        assert_eq!(read.png, None, "a picture it names but does not carry");

        let films = a_disc(&["VIDEO_TS/VIDEO_TS.IFO"]);
        assert_eq!(super::read(films.path(), Some("A Film")), None);
    }

    /// **A disc's own Windows is the one that came off it and installed
    /// nothing.** One that installed something is a game with a row; one that
    /// came off another disc is that disc's.
    #[test]
    fn a_disc_finds_the_windows_it_was_given() {
        let root = tempfile::tempdir().unwrap();
        let this = Disc {
            label: "Some Game".into(),
            uuid: "2001-02-03-04-05-06-00".into(),
        };
        let other = Disc {
            label: "Some Game".into(),
            uuid: "1999-01-01-00-00-00-00".into(),
        };
        let given = |name: &str, disc: &Disc, play: &str| {
            let mut game = crate::prefix::make(root.path(), name, None).unwrap();
            game.kept.disc = Some(disc.clone());
            game.kept.play = Some(play.into());
            game.save().unwrap();
            game
        };
        let installed = given("installed", &this, "pfx/drive_c/Game/game.exe");
        let pressing = given(
            "other-pressing",
            &other,
            "/run/media/someone/Some Game/setup.exe",
        );
        let own = given("its-own", &this, "/run/media/someone/Some Game/setup.exe");
        let games = vec![installed, pressing, own];

        assert_eq!(
            its_own_windows(&games, &this).map(|game| game.id.as_str()),
            Some("its-own")
        );
        let nobody = Disc {
            label: "Another".into(),
            uuid: String::new(),
        };
        assert_eq!(its_own_windows(&games, &nobody).map(|game| &game.id), None);
    }
}
