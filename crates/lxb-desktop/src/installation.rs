//! Installation mode: a session started for one application.
//!
//! The Linexin live medium boots into LineXinBar to install the system, and
//! the session exists to run its installer. Starting the installer beside the
//! shell, as any autostarted program, was not enough: the start screen came up
//! and animated in first, the installer appeared over it out of nowhere, and a
//! person who closed it to look round the live system had to go looking for it
//! again in a column of system tools.
//!
//! So the session can be told which application it is for. The flag travels
//! the way a session's options do — `lxb-session --installation-mode <entry>`
//! runs `lxb --shell --installation-mode <entry>`, and the compositor hands it
//! on to the shell it starts as `lxb-desktop --installation-mode <entry>` — so
//! the display manager's automatic-login line is the only thing a medium has to
//! write. An installed system never carries it. What it changes:
//!
//! * **At the start**, the application is opened on the first display — the
//!   one the session starts on, which is the one control starts on — behind a
//!   loading screen that is whole from its first frame and gone in a cut, with
//!   nothing growing out of a tile and nothing fading. Nobody pressed anything,
//!   so there is no press to acknowledge; what is kept is the screen that says
//!   what is coming. See [`crate::launch::Launch::without_a_press`].
//! * **On the bar**, it is the first tile in System, in front of Files, and the
//!   bar on every display opens standing on it — so closing it puts the person
//!   back on its tile. See [`crate::apps::scan`].
//! * **In the Home menu**, it is the first entry, by its own name — see
//!   [`crate::guide::Item::Installation`]. Pressing it is pressing that tile:
//!   brought back if it is running, opened the ordinary way if it is not.
//!
//! Everything else is the ordinary session. The Home button opens the menu over
//! it, the application closes like any other, and the rest of the bar is there
//! to be used.

use std::path::Path;

/// The application an installation-mode session was started for, and whether
/// it has been started yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mode {
    /// Its desktop entry's file name, `lxb-installer.desktop`.
    entry: String,
    /// Whether the session has opened it yet. Once, at the start, and never
    /// again by itself: after that it is opened by a press, like everything.
    started: bool,
}

impl Mode {
    /// The mode for the desktop entry a command line named, or `None` for a
    /// name that cannot be one.
    pub fn for_entry(given: &str) -> Option<Self> {
        Some(Self {
            entry: desktop_id(given)?,
            started: false,
        })
    }

    /// The desktop entry it is, by file name.
    pub fn entry(&self) -> &str {
        &self.entry
    }

    /// Whether the application is still to be opened at the start: `true`
    /// once, and `false` from then on.
    pub fn start_now(&mut self) -> bool {
        !std::mem::replace(&mut self.started, true)
    }
}

/// The desktop entry a command line names, as the file name the catalogue
/// knows it by.
///
/// Forgiving in the two ways somebody writing a display manager's
/// configuration by hand is likely to need: the `.desktop` may be left off,
/// and a whole path may be given where a name was meant — only its file name
/// is read, because an application is found in the session's own catalogue
/// and not opened from wherever a path happens to point. Nothing that cannot
/// name a file is accepted.
pub fn desktop_id(given: &str) -> Option<String> {
    let given = given.trim();
    // A folder, which `file_name` would otherwise answer with its last part.
    if given.ends_with('/') {
        return None;
    }
    let name = Path::new(given).file_name()?.to_str()?;
    let stem = name.strip_suffix(".desktop").unwrap_or(name);
    if stem.trim().is_empty() {
        return None;
    }
    Some(format!("{stem}.desktop"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name a live medium's configuration writes, and the slips the same
    /// line is likely to carry, all come to the file the catalogue has.
    #[test]
    fn an_entry_is_named_the_way_the_catalogue_knows_it() {
        for given in [
            "test-installer.desktop",
            "test-installer",
            "  test-installer.desktop ",
            "/usr/share/applications/test-installer.desktop",
        ] {
            assert_eq!(
                desktop_id(given).as_deref(),
                Some("test-installer.desktop"),
                "{given:?}"
            );
        }
        // Reverse-DNS names keep their dots.
        assert_eq!(
            desktop_id("org.example.Installer").as_deref(),
            Some("org.example.Installer.desktop")
        );
        for nothing in ["", "   ", ".desktop", "/", "/usr/share/applications/"] {
            assert_eq!(desktop_id(nothing), None, "{nothing:?}");
        }
    }

    /// The application is opened by the session once, at the start, and from
    /// then on only by a press.
    #[test]
    fn the_session_opens_it_once() {
        let mut mode = Mode::for_entry("test-installer").expect("a name");
        assert_eq!(mode.entry(), "test-installer.desktop");
        assert!(mode.start_now());
        assert!(!mode.start_now());
        assert!(!mode.start_now());
        assert_eq!(Mode::for_entry(""), None);
    }
}
