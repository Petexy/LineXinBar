//! What this helper says, and the one thing the shell and it have to agree
//! about.
//!
//! The two halves of this integration are built together and installed apart:
//! the shell is a package a machine has, and this is a package it may not, so
//! the pair that ends up talking on a given machine is not the pair that was
//! compiled together. Everything they say to each other is therefore one JSON
//! object per line on this binary's stdout, and everything either of them
//! assumes about the other is [`PROTOCOL`].
//!
//! JSON rather than a line of fields, for one reason that is worth the
//! dependency: a path is bytes with no character forbidden in it, and a
//! separator-delimited format has to decide what to do about a ROM called
//! `Tekken\t8`. The answer here is that there is no separator to collide with.
//!
//! ## What each side may do to it
//!
//! The shell reads these with `serde` and unknown fields are ignored, so a
//! **newer helper may add a field** and an older shell goes on working. The
//! reverse is what [`PROTOCOL`] is for: a helper that has had to change the
//! meaning of something already here raises the number, and a shell that does
//! not know the number puts the integration away rather than acting on a
//! record it half understands. Nothing is ever removed or repurposed at the
//! same number.

use serde::{Deserialize, Serialize};

/// The revision of everything in this file.
///
/// Raised only when a record that already exists changes what it means. Adding
/// a field is not a change to it — see the module note.
pub const PROTOCOL: u32 = 1;

/// What `lxb-retroarch probe` answers: whether there is a RetroArch on this
/// machine, and whether one could be installed if there is not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Probe {
    pub protocol: u32,
    /// The installation that would be used, or `None` for a machine with none.
    pub retroarch: Option<Installation>,
    /// Whether `flatpak` is on this machine at all, which is the whole of what
    /// decides whether the shell may offer to install anything. A machine
    /// without it is told so rather than being offered a button that cannot
    /// work.
    pub flatpak: bool,
    /// Where RetroArch keeps its own configuration, as a path this machine and
    /// the emulator both spell the same way.
    ///
    /// `None` where there is no RetroArch. What wants it is the shell: a
    /// controller has to be told to RetroArch in a file, and for a flatpak the
    /// only paths that mean the same thing on both sides of the sandbox are the
    /// ones under this. See `lxb-desktop`'s `retroarch::controllers`.
    pub config: Option<String>,
    /// Whether this helper can fetch a core — which is a fact about the helper
    /// rather than about the machine, and is why it is answered here instead of
    /// being assumed.
    ///
    /// The field an older helper does not write, and the reason the shell reads
    /// it with a default of `false`: a shell that offered to download a core to
    /// a helper with no `cores` verb would put a Yes on screen that answers
    /// with an error. See [`Fetch`].
    pub fetches_cores: bool,
}

/// One RetroArch, as found.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Installation {
    pub kind: Kind,
    /// What starts it, as an argv. The shell runs this itself — with a core
    /// and a ROM appended, or alone to open RetroArch's own interface — so
    /// that a game started from this shell is launched, watched and closed by
    /// exactly the machinery every other application on the bar is.
    pub command: Vec<String>,
    /// What it calls itself, when it would say. Shown nowhere; it is in the
    /// log, which is where "why did that core not load" starts being answered.
    pub version: Option<String>,
}

/// Which of the two kinds of installation this is.
///
/// Not cosmetic: it decides how a core's path on this disk is spelt on the
/// command line. A flatpak's own files are mounted at `/app` inside its
/// sandbox and live somewhere quite else outside it, so the path that finds a
/// core here is not the path that loads it there. See `find::Cores`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Installed by the distribution, run as `retroarch`.
    System,
    /// A flatpak in this user's own installation — what this helper installs,
    /// because it needs no authority from anybody.
    FlatpakUser,
    /// A flatpak somebody installed for the whole machine.
    FlatpakSystem,
}

/// One line of an install as it happens.
///
/// A line per change rather than a stream of everything flatpak prints: what
/// the shell has to draw is one bar and one sentence, and the rest of it is
/// this process's stderr, which is the session log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub protocol: u32,
    pub stage: Stage,
    /// How far along, 0 to 1, or `None` where flatpak has not said anything a
    /// bar could be drawn from yet. The shell draws the lights that mean
    /// "something is happening elsewhere" until a number arrives.
    pub progress: Option<f32>,
    /// One short sentence for the panel — "Downloading RetroArch" — or what
    /// went wrong, on the stage that failed.
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    /// Making sure this user can see Flathub, which is a remote they may only
    /// have system-wide.
    Remote,
    Installing,
    /// Taking it off again, which is one command and no bar: flatpak deletes
    /// what it downloaded without saying how far along it is.
    Removing,
    Done,
    Failed,
}

/// What `lxb-retroarch permit` answers: whether the emulator can now read the
/// folder the user chose.
///
/// A flatpak can only open what its filesystem permissions reach, which for the
/// Flathub build is this user's home directory and no further — so a collection
/// on an external drive is one RetroArch starts and cannot read, and it says so
/// in its own window rather than in the shell. Widening that is one command,
/// needs no authority, and affects one application: see `install::permit`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Permission {
    pub protocol: u32,
    /// Whether the folder is reachable now. True with nothing done at all for a
    /// distribution package, which is in no sandbox.
    pub permitted: bool,
    /// What was done, or what went wrong. For the log; nothing draws it.
    pub note: String,
}

/// One line of a core being fetched, as it is fetched.
///
/// The same shape as [`Progress`] and for the same reasons, with the two things
/// a core adds: *which* core a line is about, and which of several it is —
/// setting a folder up fetches one per console, and "getting the ppsspp core, 2
/// of 5" is the whole of what a panel has to say.
///
/// The last line of the stream is always `Done` or `Failed`, so that a shell
/// watching it knows the run is over without watching the process as well.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fetch {
    pub protocol: u32,
    pub stage: Getting,
    /// Which core this line is about; empty on a line about the run as a whole.
    pub core: String,
    /// How far down this core is, 0 to 1, or `None` where the server did not
    /// say how big it is.
    pub progress: Option<f32>,
    /// Which of the requested cores this is, counting from one, and how many
    /// were asked for. Both zero on the lines before any of them has started.
    pub at: u32,
    pub of: u32,
    /// One short sentence for the panel, or what went wrong on `Failed`.
    pub note: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Getting {
    /// Reading the build server's list of what it has, which happens once
    /// however many cores were asked for.
    Looking,
    Downloading,
    Done,
    Failed,
}

/// What `lxb-retroarch scan` answers: the consoles somebody's ROM folder holds,
/// and what is in each of them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Library {
    pub protocol: u32,
    /// The folder this was read from, as given. Echoed back so that a shell
    /// holding two answers can tell which is which after the setting has been
    /// changed under it.
    pub roms: String,
    /// Only the subfolders that actually hold something. A console with no ROM
    /// in it is not a column with nothing in it — it is not a column at all.
    pub consoles: Vec<Console>,
    /// Whether the folder itself could not be read, and what the filesystem
    /// said. The shell shows this on the row rather than an empty column: a
    /// drive that is not plugged in this morning must not read as "you own no
    /// games".
    pub unreadable: Option<String>,
    /// Where RetroArch reads the files a core needs beside itself, if there is
    /// a RetroArch here at all.
    ///
    /// One fact about the machine rather than about any console, and reported
    /// because the shell cannot work it out: RetroArch's own configuration may
    /// point its system folder anywhere, and a panel telling somebody to put a
    /// BIOS in the wrong place is worse than one that does not mention a place.
    ///
    /// Defaulted, so a record from an older helper still reads.
    #[serde(default)]
    pub system: Option<String>,
}

/// One subfolder of the ROM folder, and the console it turned out to be.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Console {
    /// The folder's own name, exactly as it is on the disk. What the shell
    /// files this under, because it is the one thing about a console that
    /// cannot change under it.
    pub key: String,
    /// What to call it on the bar — the console's name where this helper knows
    /// the folder, and the folder's own name where it does not.
    pub title: String,
    /// The mark to draw beside that name, as the shell looks it up:
    /// `lxb:console-nes`.
    ///
    /// `None` for a folder this helper has never heard of, which has no machine
    /// behind it to draw — and for a helper too old to have drawings at all,
    /// which is the same answer. The shell falls back to RetroArch's own mark
    /// either way, so a column always has something beside its name.
    ///
    /// A name and not a drawing: the file it names ships in this package under
    /// `share/lxb/glyphs` and the shell reads that directory at startup. See
    /// `crate::consoles::Machine::glyph`.
    #[serde(default)]
    pub glyph: Option<String>,
    /// The core that will run it, if one is installed.
    pub core: Option<Core>,
    /// The cores that could, best first, whether or not any is here. What the
    /// panel says when there is nothing to play a ROM with: naming the core is
    /// the difference between a refusal and an instruction.
    pub wanted: Vec<String>,
    /// Whether the installed core is missing the folder of files it reads
    /// beside itself — see `crate::assets`.
    ///
    /// Only ever true where `core` is `Some`, and it means the same thing to
    /// the shell as no core at all: the game will start, and it will run
    /// without the half of the emulator that draws letters. Said separately
    /// from `core` rather than by hiding the core, because what is missing is
    /// not the core and the log should not claim it is.
    ///
    /// Defaulted so that a record from a helper too old to know about this
    /// still reads, which is the same thing it would say.
    #[serde(default)]
    pub incomplete: bool,
    /// What the installed core needs beside itself and this machine has not
    /// got — see [`crate::firmware`].
    ///
    /// Empty for nearly every console, and never a thing this integration can
    /// fix: these are boot ROMs belonging to the console's maker, and the only
    /// lawful copy is one dumped from hardware the user owns. So it is said
    /// rather than acted on, and it is said *before* the press: a core that
    /// cannot boot starts and exits with nothing on the screen, which is
    /// indistinguishable from the shell being broken.
    ///
    /// Separate from [`Console::incomplete`], which is a folder of a core's own
    /// files that this helper *can* fetch and offers to.
    ///
    /// Defaulted so that a record from a helper too old to know about this
    /// still reads, which is the same thing it would say.
    #[serde(default)]
    pub needs: Vec<Need>,
    pub roms: Vec<Rom>,
}

/// One file a core cannot start without.
///
/// Both halves come from libretro's own description of the core and neither is
/// composed here: `path` is where the file goes under RetroArch's system
/// folder, and `note` is the sentence libretro writes about it — which names
/// the machine and the region, and is the only thing in this record a person
/// could take to a search engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Need {
    pub path: String,
    pub note: String,
    /// Whether it is on this disk already.
    ///
    /// Reported either way, and that is the point of the field: the shell keeps
    /// a row offering to choose where somebody's BIOS is, and that row has to be
    /// there after they have chosen one — a file put in the wrong place, or a
    /// second console's dump, is a thing to be able to do twice. What the shell
    /// says on the row and whether a game can start turn on this rather than on
    /// the entry existing.
    ///
    /// Defaulted so that a record from a helper too old to say still reads. It
    /// only ever sent what was missing, so `false` is what it meant.
    #[serde(default)]
    pub here: bool,
}

/// What one core can be set to, asked of the core itself.
///
/// One of these per installed core, written as its own line — see
/// [`crate::options`], which is also where the reason nothing here is a table
/// in this repository is written down.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreOptions {
    pub protocol: u32,
    /// The core's libretro name, without `_libretro.so`.
    pub core: String,
    /// What the core calls *itself* — "PPSSPP", "Mesen" — which is the name
    /// RetroArch files its chosen settings under and is therefore the name
    /// anything writing those settings has to spell.
    pub display: String,
    /// The groups the core sorts its settings into, in its own order. Empty
    /// for a core using a table too old to have them.
    pub categories: Vec<CoreCategory>,
    /// Every setting it declares, in its own order — which is the order its
    /// author put them in and the order RetroArch shows them in.
    pub options: Vec<CoreSetting>,
}

impl CoreOptions {
    pub const fn empty() -> CoreOptions {
        CoreOptions {
            protocol: PROTOCOL,
            core: String::new(),
            display: String::new(),
            categories: Vec::new(),
            options: Vec::new(),
        }
    }
}

/// One group of a core's settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreCategory {
    /// What the options refer to it by.
    pub key: String,
    /// What to call it on screen.
    pub title: String,
}

/// One setting a core declares.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreSetting {
    /// The key RetroArch stores it under — `ppsspp_internal_resolution`.
    pub key: String,
    /// What to call it on screen — "Rendering Resolution".
    pub title: String,
    /// The core's own sentence about what it does, where it wrote one.
    pub note: Option<String>,
    /// Which [`CoreCategory`] it belongs to, by key.
    pub category: Option<String>,
    /// The value the core uses when nobody has said otherwise.
    pub default: Option<String>,
    /// Every value it will take, in the core's own order.
    pub values: Vec<CoreValue>,
}

/// One value a setting will take.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreValue {
    /// What is written into the file — `2x`.
    pub value: String,
    /// What to call it on screen, which is the value itself where the core
    /// gave no other name for it.
    pub label: String,
}

/// A core, as the two things a caller needs it to be.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Core {
    /// Its libretro name, without `_libretro.so` — `ppsspp`, `snes9x`.
    pub name: String,
    /// The path to hand to `-L`, which is not always the path this helper
    /// found it at. See [`Kind`].
    pub path: String,
}

/// One playable thing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rom {
    /// What the row says: the file's name with its extension taken off, which
    /// is what somebody called the game when they saved it.
    pub title: String,
    pub path: String,
    /// Which folder under the console it was found in, where it was not
    /// directly in the console's own folder. Nothing is drawn from this today;
    /// it is in the record because the walk knows it and a row that has to say
    /// which of two identically named files it is would have nothing else to
    /// say it with.
    pub within: Option<String>,
    /// The cover on this disk, where this game has one — see [`crate::art`].
    ///
    /// A path rather than the picture: the shell reads it off the disk with the
    /// same worker that draws a thumbnail of any other file, and a helper that
    /// sent megabytes of PNG down a pipe of JSON would be paying twice for
    /// something already written down.
    ///
    /// Answered by every scan without anything being fetched, because it is a
    /// `stat` of a cache this helper filled earlier. Defaulted so that a record
    /// from a helper too old to know about pictures still reads, which says the
    /// same thing it would.
    #[serde(default)]
    pub boxart: Option<String>,
    /// A screenshot of it on this disk, on exactly those terms. It stands
    /// behind the whole display while the cursor is on the game, blurred — see
    /// the shell's `thumbs::Want::Snapshot`.
    #[serde(default)]
    pub snap: Option<String>,
}

/// One line of a collection's pictures being fetched, as they are fetched.
///
/// The same shape as [`Fetch`] and for the same reasons, with the two things
/// artwork adds: *which game* a line is about — by its path, which is what the
/// shell files a row under — and what that game ended up with.
///
/// Two lines per game: one as it is asked about, carrying nothing, and one when
/// it has been answered, carrying whatever was found. That is what lets covers
/// appear on the bar one at a time while a collection comes down, rather than
/// all at once at the end of it.
///
/// The last line of the stream is always `Done` or `Failed`, so that a shell
/// watching it knows the run is over without watching the process as well.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artwork {
    pub protocol: u32,
    pub stage: Picturing,
    /// The console's folder name, as [`Console::key`] spells it. Empty on a
    /// line about the run as a whole.
    pub console: String,
    /// The game's own path, which is what the shell holds its row under. Empty
    /// on a line about the run as a whole.
    pub rom: String,
    /// Which of the games asked about this is, counting from one, and how many
    /// were asked about. Both zero before any of them has started.
    pub at: u32,
    pub of: u32,
    /// The game's name, or one short sentence about the run on the lines that
    /// begin and end it.
    pub note: String,
    /// What this game now has. Both `None` while it is being asked about, and
    /// both `None` afterwards for a game nothing was found for.
    pub boxart: Option<String>,
    pub snap: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Picturing {
    /// Working out which games these are, which happens once however many were
    /// asked about.
    Looking,
    Fetching,
    Done,
    Failed,
}
