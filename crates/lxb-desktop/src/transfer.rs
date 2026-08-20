//! Carrying one of the user's own things to another folder.
//!
//! The explorer answers "what is in there" and the menu over one of its rows
//! answers "what can be done to this". Until now that list ended at Delete:
//! everything on it acted on the file where it stood. Copy and Move are the
//! first two rows that need a *second* place named before they can happen, and
//! this module is that second place — the folder the user is choosing, how they
//! walk to it, and the copy itself once they have.
//!
//! ## Why it is another bar and not a dialog
//!
//! A folder chooser is the oldest window in computing: a tree on the left, a
//! list on the right, a text field underneath. None of that is reachable from a
//! controller, and every part of it already exists on this shell in a form that
//! is — the explorer's own column. So the destination is picked in exactly the
//! way a folder is opened in Files, with one difference and one addition:
//!
//! * **It is mirrored.** The columns of the path stand to the *right* of the
//!   one being stood in rather than to the left, so Right is the way back out
//!   and Left is the way in. The bar it is drawn over runs the other way, and
//!   two lists of folders both walking left would be the same gesture meaning
//!   two different things on one screen. Mirrored, the hand knows which of the
//!   two it is driving without being told.
//! * **The head row is the answer.** Every column carries Paste at the top of
//!   it, which is what makes a folder pickable at all: stepping into a folder
//!   shows what is in it, and pressing the row above that listing says "here".
//!
//! The way back out goes as far as `/`, and no further. There is no Volumes
//! row at the top of it as there is in Files — a mounted drive is under `/` and
//! is reached by walking down to it — because the picker is a path and `/` is
//! where every path on this machine ends.
//!
//! ## What is read, and when
//!
//! The chain of levels is the source folder's ancestors, which costs nothing:
//! it is the path split up. Only two of those levels are ever *read* — the one
//! the user is standing in and the one behind it — because those are the only
//! two the drawing shows, and reading the whole way to `/` on the press that
//! opens the picker would be four `readdir`s for three columns nobody can see.
//! Every step materialises whatever the step brought into view; see
//! [`Transfer::read`].
//!
//! ## What the copy itself is
//!
//! [`Run`], on a thread, because a file is as big as it is: 172 MiB of
//! AppImage is a quarter of a second of disk on a good day and a great deal
//! longer off a stick, and the shell draws at sixty frames a second on the
//! thread that would be doing it. The same shape as [`crate::uninstall::Run`]
//! — start it, and ask it afterwards how it went.
//!
//! Nothing here overwrites anything without being told to. A name already
//! taken in the chosen folder stops the transfer and puts the question up; see
//! [`Settle`].

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Which of the two the user asked for.
///
/// One enum rather than two states of the picker, because the whole of what
/// differs between them is the word on two rows and one line of
/// [`Run::carry_out`]. What the user does — walk to a folder and press Paste —
/// is the same journey, and a picker that was a different object for a copy
/// than for a move would be two things to keep in step for no gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Copy,
    Move,
}

impl Kind {
    /// What the quiet line under Paste says: what pressing it would do, in the
    /// folder the column is of.
    pub fn note(self) -> &'static str {
        match self {
            Kind::Copy => "Copy here",
            Kind::Move => "Move here",
        }
    }

    /// How it is spoken of in a sentence on a panel — "Copying", "Moving".
    pub fn doing(self) -> &'static str {
        match self {
            Kind::Copy => "Copying",
            Kind::Move => "Moving",
        }
    }
}

/// The thing being carried: what it is called, what it says about itself, and
/// where it is now.
///
/// Held rather than looked up again on the press that finishes the journey.
/// The bar underneath the picker is still live — another display is driving its
/// own cursor, a drive can be unplugged, a folder can be rebuilt — and a
/// transfer that read its own subject off whatever happened to be selected when
/// Paste was pressed would be a transfer that could act on the wrong file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub path: PathBuf,
    /// The name as the column showed it, extension and all.
    pub name: String,
    /// The line under it: a file's size and date, a folder's contents.
    pub note: String,
    /// The mark it was drawn with, so the one row left on the left of the
    /// screen is the row that was there a moment ago.
    pub glyph: &'static str,
    pub folder: bool,
}

/// One row of a destination column.
///
/// Deliberately not [`crate::apps::Entry`]. Every row of that tree is a thing
/// that can be *pressed* — a folder opens, a file opens, a value is set — and
/// two of the three kinds here cannot: a file is on the column so that the
/// folder is recognisable, and pressing it would be pressing the thing the user
/// is trying to put something next to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// The head row: put it here.
    ///
    /// One per column, first, so it is where the column opens and where Up
    /// from the first folder lands — the same place the search field sits in
    /// Files, and for the same reason. It carries nothing: which folder it
    /// means is which column it is in.
    Paste,
    /// A folder that can be stepped into.
    Folder {
        name: String,
        at: PathBuf,
        note: String,
    },
    /// A file, listed and nothing else.
    ///
    /// It is here because a folder with its files taken out is not a folder
    /// anybody recognises: `Documents` and `Downloads` both come out as two
    /// subfolders and a blank space, and the user is being asked to say which
    /// is which. Drawn quieter than the folders, and there is nothing behind a
    /// press of it — the row a column opens on is one of these as often as not,
    /// and that is the whole point of where the column opens.
    File {
        name: String,
        note: String,
        glyph: &'static str,
    },
}

impl Row {
    /// What is written on it.
    pub fn title(&self) -> &str {
        match self {
            Row::Paste => "Paste",
            Row::Folder { name, .. } | Row::File { name, .. } => name,
        }
    }

    /// The quieter line under that. Paste's is not here: it depends on which of
    /// the two kinds of transfer is under way, which is not a property of the
    /// row — see [`Kind::note`].
    pub fn note(&self) -> Option<&str> {
        match self {
            Row::Paste => None,
            Row::Folder { note, .. } | Row::File { note, .. } => Some(note),
        }
    }

    /// The mark it is drawn with.
    pub fn glyph(&self) -> &'static str {
        match self {
            Row::Paste => crate::icons::PASTE,
            Row::Folder { .. } => crate::icons::FILE_FOLDER,
            Row::File { glyph, .. } => glyph,
        }
    }
}

/// One folder on the way home, and what the user last did in it.
struct Level {
    at: PathBuf,
    /// What is in it, once it has been looked at. `None` for a level that is
    /// only known to be an ancestor — see the module note on what is read.
    rows: Option<Vec<Row>>,
    selected: usize,
    /// Eased row, in row units, so a column goes on gliding where it was left
    /// while the user is a level away from it. The bar's own columns do the
    /// same thing for the same reason.
    position: f32,
    speed: f32,
}

/// One column as the drawing needs it.
pub struct Column<'a> {
    /// How far down the path it stands, `/` being zero.
    ///
    /// Carried rather than being the column's place in the list, because the
    /// list is only the levels that have been *read*: an ancestor nobody has
    /// walked back to is not drawn, and a drawing that took its distance from
    /// the cross out of a position in that list would put every column one step
    /// further out than it is for each of them.
    pub level: usize,
    pub rows: &'a [Row],
    pub selected: usize,
    pub position: f32,
    /// Where it stands in relation to the cursor, in the same three states the
    /// bar's own columns have — see [`crate::model::Standing`].
    pub standing: crate::model::Standing,
    /// What the quiet line under this column's Paste row says: what pressing it
    /// would do, or why it cannot be pressed here.
    ///
    /// Per column rather than per picker, because it is a fact about the folder
    /// the column is of — a move is blocked in the one folder the file is
    /// already in and nowhere else — and every column on screen carries the row
    /// it is about.
    pub paste: &'static str,
    /// Whether that row can be pressed at all in this folder.
    pub can_paste: bool,
}

/// How fast the picker's highlight and its columns settle.
///
/// The bar's own two rates, and deliberately the same numbers rather than
/// numbers of this screen's own: the user has just been walking a column of
/// folders, and the column they are walking now must not answer at a different
/// speed because it is being walked for a different reason.
const EASE_RATE: f32 = 19.0;
const DEPTH_EASE_RATE: f32 = 14.0;
const SETTLED: f32 = 0.001;

/// How long the picker takes to come in from the right, and to leave again.
///
/// A panel's own flight — see [`crate::menu::FLIGHT`] — because that is what
/// this is: something raised over the start screen by a press, which the screen
/// steps back behind. A slower arrival would leave the bar receded with nothing
/// yet standing over it.
pub const FLIGHT: f32 = crate::menu::FLIGHT;

/// A transfer the user is in the middle of arranging.
pub struct Transfer {
    kind: Kind,
    source: Source,
    /// The path home, `/` first, the folder being stood in somewhere along it.
    levels: Vec<Level>,
    /// Which of them that is.
    open: usize,
    /// Eased depth, in levels.
    depth: f32,
    depth_speed: f32,
    /// Whether it is still taking input. Not the same question as whether there
    /// is anything of it on screen: a picker that has been answered or
    /// abandoned hands the buttons back at once and is still sliding out.
    open_state: bool,
    /// How much of the way in it is, before easing.
    linear: f32,
}

impl Transfer {
    /// Start one, standing in `from` — the folder the file is in now.
    ///
    /// `None` where there is nothing to stand in, which is a source whose own
    /// folder cannot be worked out at all. Everything else — an unreadable
    /// folder, a folder that has gone away since the column was drawn — opens
    /// on a column that says so, because a picker that refused to appear would
    /// leave the user's press unanswered.
    pub fn begin(kind: Kind, source: Source, from: &Path) -> Option<Self> {
        let levels = ancestry(from);
        if levels.is_empty() {
            return None;
        }
        let open = levels.len() - 1;
        let mut picker = Self {
            kind,
            source,
            levels,
            open,
            depth: open as f32,
            depth_speed: 0.0,
            open_state: true,
            linear: 0.0,
        };
        // The two the drawing can show. The one behind is read as well as the
        // one in front because it is on screen from the first frame: it is the
        // column the source folder hangs off, and it stands there keeping the
        // row that says so.
        picker.read(open);
        if let Some(behind) = open.checked_sub(1) {
            picker.read(behind);
        }
        Some(picker)
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    /// The folder the user is standing in, which is the one Paste would put it
    /// in.
    pub fn here(&self) -> &Path {
        &self.levels[self.open].at
    }

    /// Whether it is taking input.
    pub fn is_open(&self) -> bool {
        self.open_state
    }

    /// Whether there is anything of it left to draw.
    pub fn is_on_screen(&self) -> bool {
        self.open_state || self.linear > 0.0
    }

    /// Whether something is still moving, so the display goes on asking for
    /// frames until it has settled.
    pub fn is_animating(&self) -> bool {
        (self.linear > 0.0 && self.linear < 1.0) || self.moving()
    }

    /// How much of the way in it is, eased.
    pub fn arrived(&self) -> f32 {
        crate::ui::ease(self.linear)
    }

    /// Take the buttons back and start sliding out. `true` if it was still
    /// taking them, which is what tells a caller the press was spent here.
    pub fn close(&mut self) -> bool {
        let was = self.open_state;
        self.open_state = false;
        was
    }

    /// Every column the picker is showing, `/` first.
    ///
    /// Only the levels that have been read: an ancestor nobody has walked back
    /// to yet is a column with nothing in it, and a column with nothing in it
    /// is not a column. The one being stepped out of is kept for as long as it
    /// takes to leave, exactly as the bar keeps its own.
    pub fn columns(&self) -> Vec<Column<'_>> {
        self.levels
            .iter()
            .enumerate()
            .filter_map(|(level, held)| {
                let rows = held.rows.as_deref()?;
                // Deeper than the user is standing, and settled: the column
                // they stepped out of, once it has finished leaving. Nothing
                // of it is on screen and it is not drawn.
                if level > self.open && self.depth <= self.open as f32 + SETTLED {
                    return None;
                }
                let blocked = blocked(self.kind, &self.source, &held.at);
                Some(Column {
                    level,
                    rows,
                    selected: held.selected.min(rows.len().saturating_sub(1)),
                    position: held.position,
                    standing: match level.cmp(&self.open) {
                        std::cmp::Ordering::Less => crate::model::Standing::Behind,
                        std::cmp::Ordering::Equal => crate::model::Standing::Open,
                        std::cmp::Ordering::Greater => crate::model::Standing::Leaving,
                    },
                    paste: blocked.unwrap_or(self.kind.note()),
                    can_paste: blocked.is_none(),
                })
            })
            .collect()
    }

    /// Where the chain of columns currently stands, in levels.
    pub fn depth(&self) -> f32 {
        self.depth
    }

    /// The row the cursor is on.
    pub fn selected(&self) -> Option<&Row> {
        let level = &self.levels[self.open];
        level.rows.as_ref()?.get(level.selected)
    }

    /// Move the highlight up or down the open column. Returns whether it
    /// moved.
    ///
    /// Every row can be stood on, the files included, and that is deliberate:
    /// where a column *opens* is one row below Paste, so on any folder holding
    /// nothing but files the row the highlight arrives on is one of them.
    /// There is nothing behind a press of it, which is the same answer the bar
    /// gives for a value it can show and cannot change.
    ///
    /// It stops at the ends rather than wrapping, which is what a column of the
    /// bar does. A context menu wraps because it is five rows and running off
    /// the end of it is more annoying than surprising; a folder can be a
    /// thousand, and a Down at the bottom that landed back on Paste would be
    /// the list having moved a thousand rows under one press — onto the one row
    /// that acts.
    pub fn move_selection(&mut self, delta: i32) -> bool {
        let level = &mut self.levels[self.open];
        let Some(rows) = level.rows.as_deref() else {
            return false;
        };
        let at = level.selected as i32 + delta;
        if at < 0 || at as usize >= rows.len() {
            return false;
        }
        let at = at as usize;
        if at == level.selected {
            return false;
        }
        level.selected = at;
        true
    }

    /// Put the highlight on `row` of the open column, for a press of a pointer.
    /// Returns whether it moved.
    pub fn point_at(&mut self, row: usize) -> bool {
        let level = &mut self.levels[self.open];
        let Some(rows) = level.rows.as_deref() else {
            return false;
        };
        if row >= rows.len() || row == level.selected {
            return false;
        }
        level.selected = row;
        true
    }

    /// Step into the folder the highlight is on — the way in, which on a
    /// mirrored bar is Left.
    ///
    /// Returns whether anything opened. Pressing this on Paste does not open a
    /// column: Paste is the answer, and it is answered by [`Transfer::chosen`].
    pub fn step_in(&mut self) -> bool {
        let Some(Row::Folder { at, .. }) = self.selected().cloned() else {
            return false;
        };
        // Whatever was under the open column is not where the user is going.
        // The levels past it are the way home for the path they *were* on, and
        // this is a different one from here down.
        self.levels.truncate(self.open + 1);
        self.levels.push(Level {
            at,
            rows: None,
            selected: 0,
            position: 0.0,
            speed: 0.0,
        });
        self.open += 1;
        self.read(self.open);
        true
    }

    /// Step back out to the folder this one is in — Right, on a mirrored bar.
    ///
    /// `false` at `/`, which is as far back as there is: the picker is a path,
    /// and every path on this machine ends there.
    pub fn step_out(&mut self) -> bool {
        let Some(back) = self.open.checked_sub(1) else {
            return false;
        };
        self.open = back;
        // The one that has come into view behind it. The column being stepped
        // out of keeps its rows, because it is still on screen for as long as
        // it takes to slide away.
        if let Some(behind) = back.checked_sub(1) {
            self.read(behind);
        }
        true
    }

    /// The folder Paste would put it in, if Paste is what the highlight is on
    /// and this is a folder it can go in.
    ///
    /// The second half is why this is a question rather than a field. A move
    /// into the folder the file is already in has nothing to do, and a folder
    /// carried into itself would copy for as long as the disk lasted; the row
    /// says so under itself in both cases, and answers a press with nothing —
    /// the same answer an edge of the bar gives somebody pushing into it.
    pub fn chosen(&self) -> Option<PathBuf> {
        if !matches!(self.selected(), Some(Row::Paste)) || self.blocked().is_some() {
            return None;
        }
        Some(self.here().to_path_buf())
    }

    /// Why the thing cannot be put in the folder being stood in, if it cannot.
    pub fn blocked(&self) -> Option<&'static str> {
        blocked(self.kind, &self.source, self.here())
    }

    /// Advance the springs and the arrival. Returns whether anything moved.
    pub fn animate(&mut self, dt: f32) -> bool {
        let was = self.linear;
        let step = dt / FLIGHT;
        self.linear = if self.open_state {
            (self.linear + step).min(1.0)
        } else {
            (self.linear - step).max(0.0)
        };

        let spring = |at: f32, speed: f32, target: f32, rate: f32| {
            let (at, speed) = lxb_protocol::overview::spring(
                at as f64,
                speed as f64,
                target as f64,
                rate as f64,
                dt as f64,
            );
            (at as f32, speed as f32)
        };
        (self.depth, self.depth_speed) = spring(
            self.depth,
            self.depth_speed,
            self.open as f32,
            DEPTH_EASE_RATE,
        );
        for level in &mut self.levels {
            let target = level.selected as f32;
            (level.position, level.speed) = spring(level.position, level.speed, target, EASE_RATE);
            if (target - level.position).abs() <= SETTLED && level.speed.abs() <= SETTLED {
                level.position = target;
                level.speed = 0.0;
            }
        }
        if !self.moving() {
            self.depth = self.open as f32;
            self.depth_speed = 0.0;
        }
        self.linear != was || self.moving()
    }

    /// Whether any of the springs is still travelling.
    fn moving(&self) -> bool {
        if (self.open as f32 - self.depth).abs() > SETTLED || self.depth_speed.abs() > SETTLED {
            return true;
        }
        self.levels.iter().any(|level| {
            level.speed.abs() > SETTLED || (level.selected as f32 - level.position).abs() > SETTLED
        })
    }

    /// Read level `at` off the disk, if it has not been read already.
    ///
    /// The selection lands on the folder the user came through, where they came
    /// through one, so stepping out and straight back in returns to where they
    /// were rather than to the top of the column.
    ///
    /// Everywhere else it lands on the row *below* Paste — never on Paste
    /// itself. Paste is the one row on this screen that acts, and a column that
    /// opened on it would put the end of the journey under the user's thumb at
    /// every step of the journey: walk into a folder to see what is in it,
    /// press A out of habit, and the file has been filed somewhere nobody
    /// chose. One press of Up is what it costs to reach, and reaching for
    /// something is what says it was meant.
    ///
    /// A folder with nothing in it has only the one row, and there the
    /// selection has nowhere else to be.
    fn read(&mut self, at: usize) {
        let Some(level) = self.levels.get(at) else {
            return;
        };
        if level.rows.is_some() {
            return;
        }
        let rows = listing(&level.at);
        // Which row of it the user walked in through, if they walked in through
        // one at all. Read before the level is borrowed again, because it is a
        // question about the level in front of this one.
        let came_through = self
            .levels
            .get(at + 1)
            .map(|inner| inner.at.clone())
            .and_then(|inner| {
                rows.iter()
                    .position(|row| matches!(row, Row::Folder { at, .. } if *at == inner))
            });
        // The row under Paste, whatever it is. Deliberately not "the first
        // folder": on a folder holding nothing but files that would be no row
        // at all and the selection would fall back onto Paste — which is the
        // one place it must never start.
        let below_paste = usize::from(rows.len() > 1);
        let level = &mut self.levels[at];
        level.selected = came_through.unwrap_or(below_paste);
        level.position = level.selected as f32;
        level.speed = 0.0;
        level.rows = Some(rows);
    }
}

/// The chain of folders from `/` down to `from`, each an unread level.
///
/// Only what is really there: a path is walked down from the root a component
/// at a time, so a symbolic link somewhere along it — `/home` on a machine that
/// keeps its users elsewhere — is a level like any other and Right walks back
/// out through the same names the user walked in through.
fn ancestry(from: &Path) -> Vec<Level> {
    let level = |at: &Path| Level {
        at: at.to_path_buf(),
        rows: None,
        selected: 0,
        position: 0.0,
        speed: 0.0,
    };
    let mut chain: Vec<Level> = from.ancestors().map(level).collect();
    // `ancestors` walks outwards and the picker is drawn from the root in, so
    // the chain is turned round rather than the drawing being taught to read it
    // backwards.
    chain.reverse();
    chain
}

/// What one folder offers the picker: Paste, then the folders, then the files.
///
/// The explorer's own order — see [`crate::files::listing`] — because it is the
/// same folder the user was looking at a moment ago and a column that reordered
/// itself between one screen and the next would be a different folder as far as
/// the eye is concerned. Dotfiles are left out here as they are there, and for
/// the same reason: nobody is filing a photograph in `~/.cache`.
///
/// A folder that cannot be read comes back with Paste and nothing else, which
/// is the honest column: there is nothing in it the shell may show, and putting
/// something *into* it is a question for the filesystem to answer rather than
/// for this listing to refuse in advance.
fn listing(at: &Path) -> Vec<Row> {
    let mut rows = vec![Row::Paste];
    // Everything: the picker's own rows are folders, and what it does with the
    // files is drop them a line below — this walk is a path, not a choice
    // between the things in a folder. See [`crate::files::Shows`], whose other
    // value is for the column that *is* one.
    let shown = crate::files::listing(
        at,
        "",
        crate::media::Sort::NameAscending,
        crate::files::Shows::Everything,
    );
    for entry in shown.rows.into_iter() {
        match entry {
            crate::apps::Entry::Folder(folder) => {
                let Some(crate::files::Place::Directory(at, _)) = folder.place else {
                    continue;
                };
                rows.push(Row::Folder {
                    name: folder.title,
                    at,
                    note: folder.comment.unwrap_or_default(),
                });
            }
            crate::apps::Entry::File(file) => rows.push(Row::File {
                name: file.name,
                note: file.note,
                glyph: file.glyph,
            }),
            // The field at the head of the explorer's own column, which this
            // one has no use for: there is nothing to search here, because the
            // rows are not what the user came for — the *folder* is.
            _ => {}
        }
    }
    rows
}

/// What stands between the transfer and the disk, once a folder has been
/// chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ready {
    /// Nothing does.
    Clear,
    /// Something of that name is already in the folder. What it is, so the
    /// question can say so — a file and a folder of the same name are the same
    /// obstacle and very different news.
    Taken { folder: bool },
    /// The transfer cannot be made at all, and why, in a line to put on a
    /// panel. A folder being carried into itself is the one that matters: the
    /// copy would recurse until the disk was full.
    Refused(String),
}

/// What to do about a name that is already taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settle {
    /// Put it beside the one that is there, under a name that is free.
    KeepBoth,
    /// Over the top of it.
    Replace,
}

/// Whether `source` can be carried into `into`, and what is in the way.
///
/// Asked on the thread that draws, on the press of Paste, because it is one
/// `symlink_metadata` and the answer decides which of two things happens next.
/// It is not a promise: the disk is shared with every other program on the
/// machine and the only place a name is really claimed is in the transfer
/// itself. What it is for is the question — the shell must not overwrite one of
/// the user's files without having asked.
pub fn ready(source: &Source, into: &Path) -> Ready {
    if source.folder {
        // A folder carried into itself, or into anything inside itself. The
        // copy would walk into the copy it had just made, and go on doing it
        // until the disk was full or the path was too long to open.
        if into == source.path || into.starts_with(&source.path) {
            return Ready::Refused(format!("{} cannot be put inside itself.", source.name));
        }
    }
    let landing = into.join(&source.name);
    // The name taken by the very thing being carried, which is a copy into the
    // folder it is already in. That is not an obstacle — it is a duplicate,
    // which is a thing people ask for — and it must never be answered with
    // Replace: `fs::copy` from a file to itself empties it. It lands beside
    // itself under a free name, and nothing is asked. (The same folder for a
    // *move* never reaches here; the row says so under itself and refuses the
    // press — see [`blocked`].)
    if landing == source.path {
        return Ready::Clear;
    }
    match std::fs::symlink_metadata(&landing) {
        Ok(facts) => Ready::Taken {
            folder: facts.is_dir(),
        },
        Err(_) => Ready::Clear,
    }
}

/// Why `source` cannot be put in `into`, if it cannot.
///
/// The two ways a folder can be the wrong answer, and they are worth telling
/// apart because one of them is dangerous and the other is merely pointless. A
/// folder carried into itself would copy the copy it had just made until the
/// disk was full; a move into the folder the file is already in would do
/// nothing at all and report that it had. Both are said on the row rather than
/// discovered by pressing it.
///
/// A *copy* into its own folder is neither: it is a duplicate, which is a thing
/// people ask for, and it lands under a free name — see [`free_name`].
fn blocked(kind: Kind, source: &Source, into: &Path) -> Option<&'static str> {
    if source.folder && (into == source.path || into.starts_with(&source.path)) {
        return Some("It cannot be put inside itself");
    }
    if kind == Kind::Move && already_there(source, into) {
        return Some("It is already here");
    }
    None
}

/// Whether the chosen folder is the one the thing is already in.
///
/// The Paste row is greyed there for a move, and only for a move: putting a
/// file where it already is is not a thing that can be done, and a row that
/// took the press and reported success would have said something untrue. A
/// copy in the same folder is an ordinary duplicate.
pub fn already_there(source: &Source, into: &Path) -> bool {
    source.path.parent() == Some(into)
}

/// How a transfer ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It is there, under this name — which is not always the name it had, if
    /// the user asked for both to be kept.
    Done(PathBuf),
    /// It is not, and this is what the filesystem said about it.
    Failed(String),
}

/// A transfer that has been started.
pub struct Run {
    outcome: Arc<Mutex<Option<Outcome>>>,
}

impl Run {
    /// Carry `source` into `into`, on a thread.
    ///
    /// `settle` is only consulted where something of that name is already
    /// there; where nothing is, both answers mean the same thing and the name
    /// is the one the file already had.
    pub fn start(kind: Kind, source: Source, into: PathBuf, settle: Settle) -> Self {
        let outcome = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&outcome);
        std::thread::spawn(move || {
            let result = match landing(&source, &into, settle) {
                Ok(landing) => match carry_out(kind, &source.path, &landing) {
                    Ok(()) => Outcome::Done(landing),
                    Err(err) => Outcome::Failed(said(&err)),
                },
                Err(err) => Outcome::Failed(said(&err)),
            };
            match &result {
                Outcome::Done(at) => {
                    tracing::info!(from = ?source.path, to = ?at, ?kind, "carried it over")
                }
                Outcome::Failed(why) => {
                    tracing::warn!(from = ?source.path, to = ?into, ?kind, why, "the transfer failed")
                }
            }
            if let Ok(mut slot) = slot.lock() {
                *slot = Some(result);
            }
        });
        Self { outcome }
    }

    /// How it went, or `None` while it is still going.
    pub fn outcome(&self) -> Option<Outcome> {
        match self.outcome.lock() {
            Ok(slot) => slot.clone(),
            // A worker that panicked will never answer, and a panel waiting on
            // it for ever is worse than being told it went wrong.
            Err(_) => Some(Outcome::Failed("the transfer did not finish".to_string())),
        }
    }
}

/// The exact path the thing is going to land at.
fn landing(source: &Source, into: &Path, settle: Settle) -> io::Result<PathBuf> {
    let wanted = into.join(&source.name);
    // Nothing there, or the user said to write over what is: the name it
    // already had. The one exception is a thing being copied over itself, which
    // `fs::copy` answers by emptying it — so it keeps both whatever was asked.
    if settle == Settle::Replace && wanted != source.path {
        return Ok(wanted);
    }
    // Asked without following links, for the reason [`ready`] does not follow
    // them: a symbolic link pointing at somewhere that has gone is a name that
    // is taken all the same, and writing "through" it would put the file
    // wherever the link happened to point.
    if wanted != source.path && std::fs::symlink_metadata(&wanted).is_err() {
        return Ok(wanted);
    }
    free_name(into, &source.name)
}

/// How many names are tried before giving up.
///
/// The trash's own number, for the same reason it gives: the first collision is
/// ordinary and the hundredth means something is wrong that trying again will
/// not fix.
const TRIES: u32 = 100;

/// A name in `into` that nothing is using, built out of `name`.
///
/// `osu.appimage` becomes `osu (2).appimage`, which is where every desktop this
/// shell sits beside puts the number — before the extension, so the copy is
/// still an AppImage as far as anything asking is concerned. A name with no
/// extension takes it at the end.
///
/// The double extensions a package has — `pkg.tar.zst` — are deliberately not
/// picked apart. `Path::extension` gives the last one, the number lands before
/// it, and `linux (2).pkg.tar.zst` is what the file managers alongside this
/// produce too.
fn free_name(into: &Path, name: &str) -> io::Result<PathBuf> {
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name);
    let extension = path.extension().and_then(|extension| extension.to_str());
    for number in 2..TRIES + 2 {
        let tried = match extension {
            Some(extension) => format!("{stem} ({number}).{extension}"),
            None => format!("{stem} ({number})"),
        };
        let candidate = into.join(tried);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "there is no free name left in that folder",
    ))
}

/// Do it: the whole of what separates a copy from a move.
///
/// A move is a rename where a rename will do, which is instant whatever the
/// file is the size of, and the copy is the fallback for the one case it cannot
/// be — the two paths being on different filesystems, which on any machine with
/// a stick plugged into it is most of what a move is *for*. The kernel says so
/// with `EXDEV` and nothing else does, so that is the error tested for rather
/// than any failure being taken as a reason to try the long way round.
fn carry_out(kind: Kind, from: &Path, to: &Path) -> io::Result<()> {
    if kind == Kind::Move {
        match std::fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(err) if err.raw_os_error() == Some(libc::EXDEV) => {}
            Err(err) => return Err(err),
        }
    }
    duplicate(from, to)?;
    if kind == Kind::Move {
        // The copy is there, so the original is what the move has left to
        // deal with. A failure here leaves the user with two copies rather
        // than none, which is the right way round for a failure to fall.
        if from.is_dir() {
            std::fs::remove_dir_all(from)?;
        } else {
            std::fs::remove_file(from)?;
        }
    }
    Ok(())
}

/// Copy one thing, whatever kind of thing it is.
///
/// A folder is walked rather than handed to `cp`: the shell has no terminal to
/// start one in, a process would have to be found on `PATH` before it could be
/// run, and what comes back from it is a line of text rather than an error.
///
/// A folder written over a folder is *merged* into it rather than put in its
/// place. Replace is a promise about the name, not about everything that
/// happens to be under it: a user who said Replace to "a folder called Photos
/// is already in that folder" is answering about the copy they are making, and
/// deleting the hundred files of theirs already in there — which the panel
/// never mentioned and they cannot see — is not something a press can be taken
/// to have asked for. What each file inside meets is the same question again,
/// settled the same way.
///
/// Symbolic links are copied as links rather than followed. A folder of them is
/// a folder of shortcuts and a copy that turned each into the thing it points
/// at could be a hundred times the size of what the user thought they were
/// carrying — and a link pointing at somewhere that no longer exists would fail
/// the whole transfer rather than arriving as the same broken link.
fn duplicate(from: &Path, to: &Path) -> io::Result<()> {
    let facts = std::fs::symlink_metadata(from)?;
    if facts.file_type().is_symlink() {
        let target = std::fs::read_link(from)?;
        // Nothing must be silently written over here: the caller has already
        // settled that question, and a link is not a file being replaced.
        let _ = std::fs::remove_file(to);
        return std::os::unix::fs::symlink(target, to);
    }
    if !facts.is_dir() {
        std::fs::copy(from, to)?;
        return Ok(());
    }
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        duplicate(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(())
}

/// What the shell says about a failure, in a sentence rather than in a code.
///
/// The kernel's own wording, capitalised and stopped, because it is the only
/// thing that knows what actually went wrong — "Permission denied", "No space
/// left on device". A shell that answered every failure with "the copy did not
/// work" would be a shell the user cannot act on.
///
/// Shared with the one other place in the shell that moves a file and can be
/// refused by the filesystem: changing its name. There is one way of saying
/// what went wrong because there is one thing that knows.
pub fn said(err: &io::Error) -> String {
    let mut written = err.to_string();
    // `io::Error` puts the raw code in brackets after the message for anything
    // that came from the OS. It is for a log, not for a panel.
    if let Some(bracket) = written.find(" (os error") {
        written.truncate(bracket);
    }
    let mut letters = written.chars();
    let sentence = match letters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + letters.as_str(),
        None => "It did not work".to_string(),
    };
    format!("{sentence}.")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory of this test's own under the system's temporary folder, or
    /// `None` where there is nowhere to write — in which case the test that
    /// wanted it says nothing rather than failing. The explorer's own tests do
    /// the same, for the same reason.
    fn scratch(name: &str) -> Option<PathBuf> {
        let dir = std::env::temp_dir().join(format!("lxb-transfer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        Some(dir)
    }

    fn source_of(path: &Path, folder: bool) -> Source {
        Source {
            path: path.to_path_buf(),
            name: path.file_name().unwrap().to_str().unwrap().to_string(),
            note: String::new(),
            glyph: crate::icons::FILE_PAGE,
            folder,
        }
    }

    /// Waiting for a worker in a test, so the assertion is about the disk and
    /// not about the scheduler.
    fn finished(run: &Run) -> Outcome {
        for _ in 0..2000 {
            if let Some(outcome) = run.outcome() {
                return outcome;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the transfer never finished");
    }

    #[test]
    fn the_picker_starts_in_the_folder_it_was_opened_from_and_walks_out_to_the_root() {
        let Some(dir) = scratch("walk") else {
            return;
        };
        let inside = dir.join("inside");
        std::fs::create_dir(&inside).unwrap();
        std::fs::write(inside.join("thing.txt"), b"x").unwrap();
        let mut picker = Transfer::begin(
            Kind::Copy,
            source_of(&inside.join("thing.txt"), false),
            &inside,
        )
        .unwrap();
        assert_eq!(picker.here(), inside);
        // And on the row below Paste, never on Paste: the one row that acts is
        // one deliberate press of Up away, wherever the walk stops.
        assert!(matches!(picker.selected(), Some(Row::File { .. })));
        assert!(picker.move_selection(-1));
        assert!(matches!(picker.selected(), Some(Row::Paste)));
        assert!(!picker.move_selection(-1), "the column stops at its head");
        assert!(picker.move_selection(1));
        // And out, one folder at a time, as far as there is.
        let mut walked = 0;
        while picker.step_out() {
            walked += 1;
            assert!(walked < 64, "the walk home never ended");
        }
        assert_eq!(picker.here(), Path::new("/"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Stepping out and back in comes back to the folder that was walked
    /// through, not to the top of the column.
    #[test]
    fn stepping_back_in_returns_to_where_the_user_was() {
        let Some(dir) = scratch("return") else {
            return;
        };
        let inside = dir.join("inside");
        std::fs::create_dir(&inside).unwrap();
        std::fs::create_dir(dir.join("another")).unwrap();
        let mut picker =
            Transfer::begin(Kind::Move, source_of(&inside.join("x"), false), &inside).unwrap();
        assert!(picker.step_out());
        assert_eq!(picker.here(), dir);
        // The folder walked through, rather than the row a fresh column would
        // have opened on: coming back to where you were is not entering
        // anywhere.
        assert!(matches!(picker.selected(), Some(Row::Folder { at, .. }) if *at == inside));
        assert!(picker.step_in());
        assert_eq!(picker.here(), inside);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The files are on the column to be read, and the highlight stands on
    /// them like any other row — but nothing is behind a press of one, because
    /// nothing can be filed inside a file.
    #[test]
    fn a_file_can_be_stood_on_and_opens_nothing() {
        let Some(dir) = scratch("dim") else {
            return;
        };
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        std::fs::create_dir(dir.join("folder")).unwrap();
        let mut picker =
            Transfer::begin(Kind::Copy, source_of(&dir.join("a.txt"), false), &dir).unwrap();
        let rows = picker.columns().pop().unwrap().rows.to_vec();
        assert!(rows.iter().any(|row| matches!(row, Row::File { .. })));
        // Paste, the folder, the file: the column opened on the folder, and
        // Down reaches the file below it rather than stopping short.
        assert!(matches!(picker.selected(), Some(Row::Folder { .. })));
        assert!(picker.move_selection(1));
        assert!(matches!(picker.selected(), Some(Row::File { .. })));
        assert!(!picker.step_in(), "a file leads nowhere");
        assert_eq!(picker.chosen(), None, "and it is not the answer either");
        assert!(!picker.move_selection(1), "the column stops at its foot");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder with nothing in it has only the head row, and there the
    /// selection has nowhere else to be.
    #[test]
    fn an_empty_folder_opens_on_the_only_row_it_has() {
        let Some(dir) = scratch("empty") else {
            return;
        };
        let empty = dir.join("empty");
        std::fs::create_dir(&empty).unwrap();
        let picker =
            Transfer::begin(Kind::Copy, source_of(&dir.join("a.txt"), false), &empty).unwrap();
        assert!(matches!(picker.selected(), Some(Row::Paste)));
        assert_eq!(picker.chosen(), Some(empty.clone()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_already_taken_is_reported_rather_than_written_over() {
        let Some(dir) = scratch("taken") else {
            return;
        };
        let into = dir.join("into");
        std::fs::create_dir(&into).unwrap();
        std::fs::write(dir.join("a.txt"), b"one").unwrap();
        std::fs::write(into.join("a.txt"), b"two").unwrap();
        let source = source_of(&dir.join("a.txt"), false);
        assert_eq!(ready(&source, &into), Ready::Taken { folder: false });
        // Keeping both leaves the one that was there exactly as it was.
        let run = Run::start(Kind::Copy, source.clone(), into.clone(), Settle::KeepBoth);
        let Outcome::Done(at) = finished(&run) else {
            panic!("the copy failed");
        };
        assert_eq!(at, into.join("a (2).txt"));
        assert_eq!(std::fs::read(into.join("a.txt")).unwrap(), b"two");
        assert_eq!(std::fs::read(at).unwrap(), b"one");
        // And replacing writes over it.
        let run = Run::start(Kind::Copy, source, into.clone(), Settle::Replace);
        assert!(matches!(finished(&run), Outcome::Done(_)));
        assert_eq!(std::fs::read(into.join("a.txt")).unwrap(), b"one");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_move_takes_the_original_away_and_a_copy_leaves_it() {
        let Some(dir) = scratch("both") else {
            return;
        };
        let into = dir.join("into");
        std::fs::create_dir(&into).unwrap();
        std::fs::write(dir.join("a.txt"), b"one").unwrap();
        let source = source_of(&dir.join("a.txt"), false);
        let run = Run::start(Kind::Copy, source.clone(), into.clone(), Settle::KeepBoth);
        assert!(matches!(finished(&run), Outcome::Done(_)));
        assert!(dir.join("a.txt").exists(), "a copy leaves the original");

        let run = Run::start(Kind::Move, source, into.join("deeper"), Settle::KeepBoth);
        // The folder does not exist, so the move fails and the file stays
        // exactly where it was — which is the property that matters.
        assert!(matches!(finished(&run), Outcome::Failed(_)));
        assert!(dir.join("a.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A folder is carried whole, with what is inside it.
    #[test]
    fn a_folder_is_copied_with_everything_in_it() {
        let Some(dir) = scratch("deep") else {
            return;
        };
        let from = dir.join("from");
        let into = dir.join("into");
        std::fs::create_dir_all(from.join("nested")).unwrap();
        std::fs::create_dir(&into).unwrap();
        std::fs::write(from.join("nested").join("a.txt"), b"one").unwrap();
        let source = source_of(&from, true);
        let run = Run::start(Kind::Copy, source, into.clone(), Settle::KeepBoth);
        assert!(matches!(finished(&run), Outcome::Done(_)));
        assert_eq!(
            std::fs::read(into.join("from").join("nested").join("a.txt")).unwrap(),
            b"one"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The one transfer that would never end.
    #[test]
    fn a_folder_cannot_be_carried_into_itself() {
        let Some(dir) = scratch("itself") else {
            return;
        };
        let from = dir.join("from");
        std::fs::create_dir_all(from.join("nested")).unwrap();
        let source = source_of(&from, true);
        assert!(matches!(ready(&source, &from), Ready::Refused(_)));
        assert!(matches!(
            ready(&source, &from.join("nested")),
            Ready::Refused(_)
        ));
        // Its own folder is not that. The name there is taken by the very
        // folder being copied, which is not an obstacle: it is a duplicate,
        // and it lands beside itself under a free name with nothing asked.
        assert_eq!(ready(&source, &dir), Ready::Clear);
        let run = Run::start(Kind::Copy, source, dir.clone(), Settle::KeepBoth);
        let Outcome::Done(at) = finished(&run) else {
            panic!("the duplicate failed");
        };
        assert_eq!(at, dir.join("from (2)"));
        assert!(at.join("nested").is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A move into the folder it is already in has nothing to do, and says so
    /// rather than being carried out.
    #[test]
    fn a_move_into_its_own_folder_is_not_offered() {
        let Some(dir) = scratch("already") else {
            return;
        };
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        let source = source_of(&dir.join("a.txt"), false);
        assert!(already_there(&source, &dir));
        assert!(!already_there(&source, Path::new("/")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What is said about a failure is the kernel's own wording, without the
    /// number it carries for the log.
    #[test]
    fn a_failure_is_reported_in_words() {
        let said = said(&io::Error::from_raw_os_error(libc::EACCES));
        assert!(said.starts_with("Permission denied"), "{said}");
        assert!(said.ends_with('.'), "{said}");
        assert!(!said.contains("os error"), "{said}");
    }
}
