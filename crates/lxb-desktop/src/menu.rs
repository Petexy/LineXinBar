//! The context menu: the short list of things that can be done to whatever is
//! selected right now.
//!
//! Everything else the shell draws is a surface with its own layout — the bar,
//! the guide's sidebar, the keyboard. This is the one piece of furniture that
//! belongs to *no* screen in particular. It is raised over whichever one asked
//! for it, out of whatever control it is about, and it takes every button until
//! it is answered.
//!
//! So it is written as a component rather than as another screen. Whoever
//! raises it supplies three things and nothing else:
//!
//! * an **anchor** — the rectangle of the control the menu is about, in display
//!   coordinates, which is where the panel grows out of and folds back into;
//! * an optional **title**, which is what the anchor *is*, not what the menu
//!   does;
//! * the **entries**, built one line each.
//!
//! ```ignore
//! menu.open_at(
//!     tile_rect,
//!     Some(app.name.clone()),
//!     vec![
//!         Entry::new(Command::Placeholder("show-information"), "Show Information"),
//!         Entry::new(Command::Placeholder("hide"), "Hide from the start screen").disabled(),
//!         Entry::new(Command::Placeholder("remove"), "Remove").group(1).grave(),
//!     ],
//!     ui::context_menu_rows_that_fit(height),
//! );
//! ```
//!
//! Adding an entry later is one more line in that list plus one variant of
//! [`Command`]; raising the menu somewhere new is one more call. Neither costs
//! anything here, in the drawing, or in the input handling, and that is the
//! whole design.
//!
//! Nothing in this module knows how tall a display is or how the panel is
//! drawn. The one thing it has to be told is that last argument — how many rows
//! fit on the screen it was raised on — because a list longer than that
//! scrolls, and where the list is scrolled to is state, not layout. It is an
//! argument rather than something to remember to set, so that a menu raised
//! from somewhere new cannot be one that runs off the bottom of the display.

use std::time::Instant;

use crate::system::Level;

/// What choosing an entry asks the shell to do.
///
/// One variant per command the shell's context menus can carry. An enum rather
/// than a boxed callback because the menu is raised from one place and answered
/// in another, and a name that has to be matched somewhere is a name the
/// compiler can check: adding a command is a variant here and an arm wherever
/// menus are answered, and forgetting the second half does not compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Put up what the shell knows about the application the menu is about.
    Information,
    /// Ask whether to remove it from the machine.
    Uninstall,
    /// Answer that question with yes.
    ///
    /// Separate from [`Command::Uninstall`] rather than the same command
    /// arriving a second time: one of these opens a question and the other
    /// destroys something, and a shell where those are one name is a shell one
    /// mis-routed press away from removing an application nobody asked about.
    ConfirmUninstall,
    /// Put RetroArch on this machine, the user having said yes.
    ///
    /// Its own command for the reason [`Command::ConfirmUninstall`] is: one
    /// press opens a question and this one downloads several hundred
    /// megabytes, and a shell where those are one name is a shell one
    /// mis-routed press away from doing it unasked.
    InstallRetroArch,
    /// Fetch the core a game needs, from the panel raised by pressing that
    /// game. See `Shell::say_no_core`.
    GetCore,
    /// Look at a different kind of file, from the menu of the panel an
    /// application's file question is being answered in.
    ///
    /// `None` is everything on the disk, which is the row under the kinds the
    /// application named. It is a command of its own rather than a kind
    /// numbered past the end of the list, because "show me the whole disk" is a
    /// different sentence from "show me images" and the two are answered by
    /// different halves of [`crate::picker::Picker::show_kind`].
    PickKind(Option<usize>),
    /// Step into the list of them, from the row that stands for all of them.
    ///
    /// The kinds used to be the top of that menu, one row each, with Sort and
    /// Show hidden files under them — and an application offering five filters
    /// pushed everything else off the bottom of a short panel. One row that
    /// says what is showing and steps into the list is what a menu does with a
    /// list that is not a fixed length; the same shape [`Command::Sort`] has.
    PickKinds,
    /// Read somebody's ROM folder again, from the menu over the RetroArch row.
    ///
    /// The counterpart of Steam's *Refresh the library*, and it exists for the
    /// same reason: the shell reads that folder when it starts and when the
    /// folder is chosen, and a game copied onto the machine in between is a
    /// game the bar has no way of hearing about.
    RetroArchRescan,
    /// Fetch whatever cores the games in that folder need and this machine has
    /// not got — every console at once, rather than one game's at a time.
    ///
    /// Its own command rather than [`Command::GetCore`] arriving from a
    /// different row: that one is answered by a panel about *one* game and
    /// knows which core it is for, and this one is about the whole collection.
    RetroArchCores,
    /// Fetch the cover and the screenshot of the game the menu is about, from
    /// libretro's collection.
    ///
    /// Its own command rather than [`Command::RetroArchCores`] wearing another
    /// name: that one is about a whole collection and downloads emulators, and
    /// this one is about the row under the cursor and downloads two pictures.
    ///
    /// It is on a game's menu at all because the shell's own attempt can miss:
    /// a game is matched to libretro's name for it by reducing both to what two
    /// people spelling it differently would agree on, and somebody who called
    /// their file `smb.nes` has written down less than that needs. This is the
    /// row that says "try again for this one" once they have called it what it
    /// is.
    RetroArchArt,
    /// The same for every game of one console at once, from the menu over that
    /// console's row.
    ///
    /// Its own command rather than [`Command::RetroArchArt`] raised from
    /// another row, and the difference is what the press costs: that one is
    /// about the game under the cursor and fetches two pictures, and this one
    /// is about a shelf and fetches two hundred. A single name for both would
    /// be one mis-routed press away from a collection coming down the wire
    /// because somebody asked about one game.
    RetroArchConsoleArt,
    /// Take the cursor to the settings page of the emulator that plays this
    /// console — Settings > Games > RetroArch > the emulator's own name.
    ///
    /// On the menu over a console and over one of its games, and it means the
    /// same thing on both: what a game looks like is decided by the emulator
    /// running it, and that page is where those settings are. It moves the bar
    /// rather than opening anything, which is why it is one command and not
    /// two — a row that took the user somewhere else depending on which of two
    /// rows raised it would be two rows sharing a name.
    RetroArchCoreSettings,
    /// Choose a picture of the user's own for this game's cover.
    ///
    /// What libretro has is what nearly every game gets, and the two cases this
    /// answers are the ones it cannot: a game whose name is too far from the
    /// database's for any match, and a game whose published cover is not the
    /// edition somebody owns.
    RetroArchCover,
    /// The same for the picture that stands behind the display while the cursor
    /// is on the game.
    ///
    /// Its own command rather than a cover with a flag on it, for the reason
    /// every other pair in this list is two names: the two pictures are drawn
    /// in different places and are chosen in two separate journeys, and a
    /// single name would be one mis-routed press away from a screenshot
    /// arriving where a cover was asked for.
    RetroArchBackground,
    /// Take away the picture somebody chose for this game, so the row goes
    /// back to what libretro published — or to its console's mark, where
    /// libretro has nothing.
    ///
    /// Its own command rather than [`Command::RetroArchCover`] pressed a second
    /// time, and the menu shows one or the other and never both: a row that
    /// meant "choose" on a game with no picture of its own and "forget the one
    /// I chose" on a game with one would be a row whose meaning the user has to
    /// work out from what the row beside it is wearing.
    RetroArchDropCover,
    /// The same for the picture behind the display.
    RetroArchDropBackground,
    /// Start RetroArch's own interface.
    ///
    /// The shell hides RetroArch from the list of installed applications and
    /// stands its own row in its place, so without this row there is no way to
    /// reach the emulator's own screens — and there are things only they can
    /// do. Steam's row makes the same offer for the same reason.
    RetroArchOpen,
    /// Open the picker that says where somebody's games are.
    ///
    /// Raised from the panel that explains what the folder has to look like,
    /// which is the one place that explanation belongs — a row's line under it
    /// has room for the folder's name and not for a sentence about how to sort
    /// a collection.
    ChooseRomsFolder,
    /// Open the picker that says where somebody's BIOS dumps are.
    ///
    /// Raised from the panel over a game that cannot start without one, which
    /// is the one moment anybody cares: a console needing a file nobody may
    /// distribute is a sentence in a settings page until the day it stops a
    /// game, and then it is the only thing on screen worth answering.
    ChooseBiosFolder,
    /// Take RetroArch off this machine, with everything it and this
    /// integration kept.
    ///
    /// Raises a panel spelling out what goes before anything happens — see
    /// `Shell::offer_to_remove_retroarch`. The row is on the menu over the
    /// RetroArch row under Games, and only where there is a RetroArch to
    /// remove.
    RemoveRetroArch,
    /// They read that panel and said yes.
    ///
    /// Its own command rather than the same one pressed twice, because the two
    /// presses mean different things and the difference is a program being
    /// deleted: one asks a question and one answers it. Nothing but the panel's
    /// own button carries this.
    ReallyRemoveRetroArch,
    /// Stop looking for a BIOS, and put the question away with the panel.
    ///
    /// Not [`Command::Dismiss`], because there is something to forget: which
    /// console every folder chooser on the bar is currently answering for. See
    /// `Shell::cancel_bios_folder`.
    CancelBiosFolder,
    /// Hand what has been typed into the password field to the thing waiting
    /// for it. The one command that carries nothing with it: what was typed
    /// lives where the shell can look after it, not in a menu entry.
    SubmitPassword,
    /// The same, for the field on the panel `polkitd` raised — see
    /// [`crate::polkit`].
    ///
    /// Its own command rather than [`Command::SubmitPassword`] arriving from a
    /// different panel, because the two hand what was typed to different
    /// things: one goes to `sudo` and destroys an application, the other goes
    /// to PAM and proves who is at the machine. A single name for both would be
    /// one mis-routed press away from answering the wrong question with a
    /// password meant for the other.
    Authenticate,
    /// The same again, for the field on the panel that collects a wireless
    /// network's password — see [`crate::network`].
    ///
    /// Its own command for the reason [`Command::Authenticate`] is its own: the
    /// three panels hand what was typed to three different places, one of them
    /// being a daemon that will put it on the air, and a single name for all of
    /// them would be one mis-routed press away from broadcasting the password
    /// that proves who is at the machine.
    JoinNetwork,
    /// Answer the question BlueZ has stopped a pairing on — see
    /// [`crate::bluetooth`].
    ///
    /// One command for four questions, where the three above it are one command
    /// each, and the difference is the one this list is sorted by: what a panel
    /// *does with the answer*. Those three hand what was typed to three
    /// different places — `sudo`, PAM, and a daemon that will put it on the air
    /// — so a single name for them would be one mis-routed press away from
    /// broadcasting the password that proves who is at the machine. These four
    /// all hand their answer to the same agent, about the same pairing, in the
    /// same moment; whether it is a yes or six digits is a property of the
    /// question and not of where it goes.
    PairDevice,
    /// Put what was typed into the value the field on this panel is for — an
    /// address, a router, a list of name servers. See [`crate::network`].
    ///
    /// Its own command for the reason every other field's is: what a panel does
    /// with what was typed is the whole of what separates these, and one name
    /// for all of them would be one mis-routed press away from writing an IP
    /// address where a password was asked for.
    SetValue,
    /// Start it — or come back to it, if it is already running.
    Launch,
    /// Send the selected window to the display after this one, or the one
    /// before it.
    ///
    /// Two commands rather than one carrying a direction, because a menu entry
    /// is a thing the user chose by name: the row that says "next" and the row
    /// that says "previous" are as different as any other two rows, and a
    /// single command with a sign in it would be one place for the wrong sign
    /// to arrive from.
    MoveToNextDisplay,
    MoveToPreviousDisplay,
    /// Hand the floating window to the pointer: it follows until it is clicked
    /// down. The compositor's to carry out — see `lxb_shell_v1.pip_command`.
    PipMove,
    /// The same, resizing it from the corner with the most room to grow into.
    PipResize,
    /// Move it to the display beside the one it is on.
    ///
    /// Its own commands rather than [`Command::MoveToNextDisplay`] arriving
    /// from another row: that pair acts on the card the *guide* has selected,
    /// and this window is deliberately not in that deck. One name for both
    /// would be one mis-routed press away from moving whichever application
    /// happened to be selected behind the video.
    PipToNextDisplay,
    PipToPreviousDisplay,
    /// Put it back in the column: the size the Settings page asks for, in the
    /// corner it asks for, wherever it had been dragged or pulled to.
    ///
    /// The one row here that undoes the other rows rather than doing something
    /// of its own. It is the compositor's to carry out for the reason all of
    /// these are: where a window is on a display is a fact only the compositor
    /// has.
    PipRealign,
    /// Take it out of its corner and give it the whole display, as an ordinary
    /// application window: listed in the guide, holding the keyboard, closed and
    /// switched to like any other.
    ///
    /// Its counterpart is [`Command::FloatWindow`], and they are one request in
    /// two directions — see `lxb_shell_v1.set_window_floating`. A video sent
    /// this way has no floating menu left to be got back with, so the row that
    /// sends it back lives on the menu of the deck it has just joined.
    PipFullScreen,
    /// The other direction: the window the *guide* has selected leaves the
    /// layout and goes to sit in a corner, as a browser's picture-in-picture
    /// window does — whatever it happens to call itself.
    ///
    /// Drawn only where the session has floating windows at all. A window given
    /// a corner on a session that has switched them off would be one with no
    /// menu to be got out of the corner with.
    FloatWindow,
    /// Ask the floating window to close, which puts the video back in the page
    /// it came out of.
    ///
    /// Not what the guide's own Close does: that one ends the *application*
    /// behind the window, and the application here is a browser with the rest
    /// of somebody's session in it.
    PipClose,
    /// Photograph the selected window and put the picture with the user's
    /// other ones.
    Screenshot,
    /// Let the application that is asking see one of this session's displays,
    /// by its place in the shell's own list of them.
    ///
    /// The display travels in the command because the question offers one row
    /// per screen, and which row was pressed is the whole answer. Its
    /// counterpart is deliberately a separate command rather than an absent
    /// one: refusing is a thing the user *did*, and it is answered as such.
    ShareDisplay(usize),
    RefuseShare,
    /// Silence the application this mixer row is about, or bring it back.
    ///
    /// The one command that names its subject. Every other row here is about
    /// whatever was selected when the menu was raised, which the shell can look
    /// up again when the row is chosen; a mixer lists several applications at
    /// once, so the row has to say which of them it is.
    MuteApplication(u32),
    /// The same for the shell's own sounds — navigation, keys, launches and
    /// Start's background music.
    ///
    /// Its own command rather than a number that stands for one more program,
    /// because the shell is not one: it has no stream in the mixer to be found
    /// among the applications, and it is the one thing on the list whose sound
    /// this session decides for itself.
    ///
    /// Deliberately *not* the session's output. That is what the volume bar in
    /// the sidebar moves, which is why the bar is there without the mixer
    /// having to open; a row that turned the whole machine down would be the
    /// same control twice, and would leave the shell's own sounds with none.
    MuteShell,
    /// Put away the announcement this row is about — see [`crate::notify`].
    ///
    /// It names its subject for the same reason the mixer's rows do, and it is
    /// the whole of what a row in that panel does: the list is a thing to be
    /// read and cleared, and a press is the user saying they have read this
    /// one. The panel stays up, because the answer is the row going.
    DismissNotification(u32),
    /// Open the announcement this row is about: what it said in full, and the
    /// buttons the program offered with it.
    ///
    /// A row carries this instead of [`Command::DismissNotification`] only
    /// when there is something to open — an announcement with no buttons has
    /// nothing behind it but a list of one, and making the user step into that
    /// to get back out of it would be a panel wasting their time.
    ShowNotification(u32),
    /// Press one of those buttons: the announcement, and which of its actions
    /// by position.
    ///
    /// By position rather than by name because a command is `Copy` and a name
    /// is a `String`. The position is only ever read back out of the very
    /// announcement in the first half of the pair, so the two cannot drift
    /// apart the way an index into a separate list would.
    InvokeNotification(u32, usize),
    /// Put away all of them at once.
    ///
    /// Its own command rather than the row above arriving several times, for
    /// the reason [`Command::ConfirmUninstall`] is its own: a row that clears
    /// one thing and a row that clears everything are as different as any two
    /// rows, and one name for both would be one mis-routed press away from
    /// emptying a list somebody was reading.
    DismissNotifications,
    /// Open one of the user's own files — a song, a film, a photograph — in
    /// whatever their desktop already opens that kind of file with.
    ///
    /// Not [`Command::Launch`], although the shell carries both out the same
    /// way. What the row says is "Open", the thing it acts on is a file rather
    /// than an installation, and the two menus that carry them have nothing
    /// else in common: keeping one name for both would mean the day one of them
    /// grows a step the other must not take, there is nowhere to put it.
    Open,
    /// Ask *which* application, instead of taking the answer the desktop has
    /// already given.
    OpenWith,
    /// Open it with the `n`th of the applications that list offered.
    ///
    /// An index rather than a name, for the reason [`Command::MuteApplication`]
    /// carries a key: a command is copied around and matched on, the list it
    /// indexes is the one the shell built when it raised the menu, and a menu
    /// that is not up has no list to index.
    OpenWithHandler(usize),
    /// Ask *which* application again, this time out of everything installed
    /// rather than out of what says it opens this type.
    ///
    /// Its own command rather than [`Command::OpenWith`] with a flag, because
    /// what the two lists are *for* is different and the rows below say so.
    /// That one answers "which of these should open every file of this kind
    /// from now on" and every row of it writes the answer down. This one
    /// answers "open this one file with that", which is the only question a
    /// file with no extension can be asked — nothing declares it, so the list
    /// above it is empty and the type it would be recorded under is
    /// `application/octet-stream`, which is every unrecognised file on the
    /// machine.
    OpenWithOther,
    /// Open the selected file with the `n`th of the applications *that* list
    /// offered.
    ///
    /// An index rather than a name, for the reason
    /// [`Command::OpenWithHandler`] carries one — and a separate command from
    /// that one for the reason above: this press starts a program and that one
    /// edits a preferences file.
    OpenWithApp(usize),
    /// Turn on, or off, the row at the head of that list which says the
    /// application chosen next should also become the default for the type.
    ///
    /// A tick that holds the panel rather than a command that acts. It changes
    /// nothing on its own — what it changes is what the *next* press means —
    /// which is why it can be pressed twice with no consequence, and why it is
    /// safe for it to be the row the list opens on.
    OpenWithAlways,
    /// Put the selected file in the trash — after asking.
    Delete,
    /// Answer that question with yes. Separate from [`Command::Delete`] for the
    /// same reason [`Command::ConfirmUninstall`] is separate: one of these
    /// opens a question and the other takes somebody's photograph off the
    /// disk.
    ConfirmDelete,
    /// Carry the selected file or folder somewhere else on the disk: leaving a
    /// copy of it where it is, or not.
    ///
    /// Two commands rather than one carrying which, and for the reason the two
    /// display rows are two commands: they are rows the user picked by name.
    /// One of them ends with the thing still where it was and the other does
    /// not, which is the largest difference there is between two rows of a
    /// menu, and a single command with a flag in it would be one place for the
    /// wrong flag to arrive from.
    ///
    /// Neither carries its subject. The menu is about whatever was selected
    /// when it was raised, exactly as [`Command::Delete`] is, and what the
    /// press does is open the picker — which takes a copy of the subject there
    /// and then. See [`crate::transfer`].
    Copy,
    Move,
    /// Answer the question a name already taken in the chosen folder puts up:
    /// write over what is there, or put both side by side.
    ///
    /// Their own commands rather than [`Command::Copy`] arriving a second time,
    /// for the reason [`Command::ConfirmDelete`] is its own: one of these
    /// destroys one of the user's files and the other cannot, and a shell where
    /// those are one name is a shell one mis-routed press away from overwriting
    /// something nobody asked about.
    ReplaceFile,
    KeepBothFiles,
    /// Put a trashed thing back where it came from.
    ///
    /// No question in front of it, unlike every other row that moves one of the
    /// user's files: this is the one act in the shell that *undoes* a loss.
    /// The worst it can do is put a file somewhere the user did not want it,
    /// and the row that put it in the trash is still there to press again.
    Restore,
    /// Destroy a trashed thing for good, and the question that leads to it.
    ///
    /// Two commands for the reason [`Command::Delete`] and
    /// [`Command::ConfirmDelete`] are two: one of them opens a question and the
    /// other takes somebody's photograph off the disk with nothing left to get
    /// it back from. This pair is the graver of the two — Delete leaves the
    /// file in the trash, and there is nowhere further down than this.
    Purge,
    ConfirmPurge,
    /// Yes to the question the row at the head of the Trash column raises: get
    /// rid of everything in there.
    ///
    /// Its own command rather than [`Command::ConfirmPurge`] arriving from
    /// another panel, for the reason [`Command::DismissNotifications`] is its
    /// own: a press that destroys one thing and a press that destroys four
    /// hundred are as different as any two presses in this shell, and one name
    /// for both would be one mis-routed answer away from emptying a trash
    /// somebody was reading.
    ///
    /// It has no opening half. The question is raised by pressing the row
    /// itself rather than by a row of a menu — that head row *is* the command,
    /// the way the search field at the head of a folder is — so there is
    /// nothing for a second name to be carried on.
    ConfirmEmptyTrash,
    /// Put the caret on the selected row's name, so it can be changed.
    ///
    /// It opens a field rather than doing anything, which is why it is one
    /// command and not two: what comes back is not an answer the menu has to
    /// carry — the row itself collects it, and the keyboard is what says so.
    /// See [`crate::main::Renaming`] for why the row goes on saying what the
    /// file is really called while it is being typed over.
    Rename,
    /// Turn the column into one that is being marked, with the row the menu was
    /// raised over already ticked.
    ///
    /// What changes is what a press *means* in that column: it ticks and
    /// unticks rows instead of opening them, the menu raised while it is on is
    /// about the whole set, and the row at the head says how many there are and
    /// is the way back out. See [`crate::marks`].
    ///
    /// It is on the menu rather than being a button of its own because there is
    /// no button of its own to be had: every control a pad offers this shell
    /// already means something in a column, and a mode nobody can find is worse
    /// than a row nobody presses.
    SelectMultiple,
    /// Tick every row of the column that can be ticked.
    SelectAll,
    /// Take every tick off, leaving the marking on.
    ///
    /// Its own command rather than [`Command::SelectAll`] pressed again, and
    /// deliberately not the way *out* of the marking either: those are three
    /// different things — one fills the set, one empties it, one ends the
    /// picking — and a single row that did whichever of them fitted would be a
    /// row whose meaning the user works out afterwards.
    ClearMarks,
    /// Put everything ticked in the trash — after asking.
    ///
    /// Its own command beside [`Command::Delete`], for the reason that one is
    /// separate from [`Command::ConfirmDelete`]: the two act on different
    /// things — one on the row the menu was raised over, one on a set the user
    /// built — and one name for both would be one mis-routed press away from
    /// forty files going where one was meant to.
    DeleteMarked,
    ConfirmDeleteMarked,
    /// Carry everything ticked somewhere else on the disk, leaving a copy of it
    /// where it is, or not.
    ///
    /// Their own commands beside [`Command::Copy`] and [`Command::Move`], on
    /// the same argument, and neither carries its subject: the set is whatever
    /// is ticked when the press lands, and the picker takes a copy of it there
    /// and then — see [`crate::transfer::Transfer::sources`].
    CopyMarked,
    MoveMarked,
    /// Put everything ticked back where it came from.
    ///
    /// No question in front of it, for the reason [`Command::Restore`] has
    /// none: it is the one act in this shell that undoes a loss.
    RestoreMarked,
    /// Destroy everything ticked for good, and the question that leads to it.
    ///
    /// The gravest press in the shell after emptying the trash, and it is two
    /// commands for the reason that one is: one of them opens a question and
    /// the other takes a set of somebody's files off the disk with nothing left
    /// to get them back from.
    PurgeMarked,
    ConfirmPurgeMarked,
    /// Ask what order the column should be listed in.
    Sort,
    /// Turn the names that begin with a dot on, or off, in every folder this
    /// shell lists.
    ///
    /// A tick that holds the panel, on the terms [`Command::OpenWithAlways`] is
    /// one: pressing it changes what is on the column behind the menu, and a
    /// panel that folded away would take the answer with it before it could be
    /// read. Unlike that one it acts at once and is written down — it is a
    /// preference about reading a listing, the same kind of thing
    /// [`Command::Sort`] leads to, which is why it stands beside Sort in the
    /// band that is about the column rather than about the file.
    ///
    /// One command and not a pair, because there is only one of it: what it
    /// answers is which way it is being turned, and the switch itself knows.
    ShowHidden,
    /// List it in this one.
    SortBy(crate::media::Sort),
    /// Raise the panel that signs somebody in to Steam, on its first question.
    ///
    /// Also what the Try again row of a failed sign-in is: starting over is
    /// the same thing as starting, and a second name for it would be a second
    /// place to keep the same list of ways in.
    SteamSignIn,
    /// Sign in by photographing a code, or by typing an account name and a
    /// password. Two commands rather than one carrying a choice, for the
    /// reason the two display rows are two commands: they are rows the user
    /// picked by name, and they lead to different panels.
    SteamWithQr,
    SteamWithPassword,
    /// Hand over whatever the sign-in panel is asking for — an account name, a
    /// password, a Steam Guard code.
    ///
    /// One command for all three, unlike [`Command::SubmitPassword`] and
    /// [`Command::Authenticate`], which are deliberately separate because they
    /// hand what was typed to different programs. This one has a single
    /// destination: the sign-in that raised the panel. Which question is being
    /// answered is which stage that sign-in is on, and there is nowhere else
    /// an answer could go — see [`crate::steam::Steam::submit`].
    SteamSubmit,
    /// Give up on a sign-in that is under way.
    SteamCancel,
    /// Stop watching Valve's client install itself, and let it carry on.
    ///
    /// Deliberately not a [`Command::SteamCancel`]: nothing is given up. Half a
    /// gigabyte is coming down and stopping it would throw away what had
    /// arrived, so the panel goes and the install does not. Whoever pressed it
    /// is told when it finishes, and pressing the Steam row again brings the
    /// panel back. See [`crate::steam::Steam::let_the_setup_run_in_the_background`].
    SteamSetupInBackground,
    /// Try installing Valve's client again after an install that failed.
    ///
    /// Its own command rather than [`Command::SteamSignIn`], which is the Try
    /// again of a failed *sign-in*: the two look alike on a panel and are not
    /// the same request, and one of them can only be made on a machine where
    /// Steam is not installed.
    SteamSetUpAgain,
    /// Give up the stored session, so this machine stops being signed in.
    SteamSignOut,
    /// Ask Steam for the library again, now.
    SteamRefresh,
    /// Ask what order the Steam column should be listed in, and list it in this
    /// one.
    ///
    /// The pair [`Command::Sort`] and [`Command::SortBy`] are, doing the same
    /// two things one column further along the bar — and deliberately not those
    /// two commands with a wider argument. What is being ordered is a library
    /// of games rather than a shelf of files: the orders are different orders,
    /// they are written down under a different key, and the one thing the two
    /// have in common is the word on the row. A single pair would have to ask,
    /// at the moment it was carried out, which kind of column raised it — which
    /// is the question having two names already answers.
    SteamSort,
    SteamSortBy(lxb_steam::library::Sort),
    /// Put the account into one of Steam's statuses, off the friends panel.
    ///
    /// The one row anywhere in this shell that writes something to somebody
    /// else's service about the person using it, which is why it names the
    /// state rather than a verb: what is chosen is what the row says, and what
    /// the row says is what everybody on the list will see.
    SteamStatus(lxb_steam::Presence),
    /// Put up what this session's Steam is doing, as a panel to read.
    ///
    /// A panel rather than a page, for the reason System information is one:
    /// what is behind it is a dozen named values with an answer beside each,
    /// and a column offering them as rows would be a list somebody has to walk
    /// down to read a value at a time. See `Steam::diagnostics`.
    SteamDiagnostics,
    /// Throw away the pictures this shell fetched for the Steam column.
    ///
    /// Offered from the diagnostics panel, beside the number it is about, and
    /// nowhere else — it is a thing to do when something is wrong with a
    /// picture or when a disk is full, and neither is a row anybody should meet
    /// while walking their library. Nothing is lost that cannot be fetched
    /// again.
    SteamForgetArtwork,
    /// Take away the marker that makes Valve's client expose its interface.
    ///
    /// Offered from the diagnostics panel, beside the line that says the
    /// interface is open, and only when there is a marker to take away that
    /// this shell did not make itself. Everything about why that is a button
    /// rather than something the shell does on its own is in
    /// `lxb_steam::webui::shut_the_interface`: the shell will not turn off a
    /// thing somebody else turned on, and being asked to is not the shell
    /// deciding.
    SteamShutTheInterface,
    /// Fetch a game the account owns and this machine does not have.
    ///
    /// Carries the app rather than acting on whatever is selected, because the
    /// answer arrives after a round trip and the selection may have moved by
    /// then — and because the same command is offered from a dialog, where
    /// there is no selection to speak of.
    /// Try one game again, out of the panel that said it did not start.
    ///
    /// Carries the app for the reason [`Command::SteamInstall`] does: the panel
    /// is read after the fact and the bar may have moved on, so the row cannot
    /// mean "whatever is selected".
    /// Ask before signing out, rather than doing it.
    ///
    /// The row it is on says "Sign out of LineXinBar" and that is exactly what
    /// it does — it is not the whole of signing out of Steam on this machine,
    /// and the panel behind this is where the difference is said.
    SteamSignOutNow,
    SteamPlayAgain(u32),
    /// Move Valve's client to this session and this account, having asked, and
    /// start the game that was waiting on it.
    ///
    /// The only row in the shell that may end a Steam this session did not
    /// start. It is offered from one panel and nowhere else — see
    /// `Shell::offer_to_move_steam_here` — because the question it is the
    /// answer to is what makes it allowed.
    SteamMoveClientHere(u32),
    SteamInstall(u32),
    /// Stop fetching one, and take away what had arrived. There is no
    /// resuming, so pressing Install again starts over.
    SteamStopInstalling(u32),
    /// Hand one game's whole install to Steam's own window.
    ///
    /// Offered only after the silent install has come back saying the game
    /// wants something from the person — an agreement to accept, most often —
    /// which is the one thing this shell will not answer on anybody's behalf.
    /// Carries the app because it is offered from a dialog that may be
    /// answered long after the cursor has moved on.
    SteamInstallWithSteam(u32),
    /// Ask whether to take one game off the disk.
    ///
    /// Carries the app for the reason [`Command::SteamInstall`] does: it is
    /// offered from a dialog as well as from the menu, and by the time it is
    /// pressed there the selection is not what the answer is about.
    SteamUninstall(u32),
    /// Take it off, the question having been answered.
    ///
    /// Deliberately a second command rather than the same one twice. Nothing
    /// past this point asks anything — Valve's client is told not to put its
    /// own confirmation up, which is the whole point — so this is the press
    /// that deletes a game, and it exists only on the panel that asked.
    SteamUninstallNow(u32),
    /// Start Valve's client, because something the user is looking at needs one
    /// and there is not one running.
    ///
    /// Steam does not take the game as an argument — it comes up and gets on
    /// with whatever its own manifests say is outstanding — but the app is
    /// carried anyway, because the shell has to know **what it started the
    /// client for**. A session that is not to leave Steam running has to close
    /// it again when that work is done, and "that work" is this game. See
    /// `Shell::close_steam_when_the_press_is_over`.
    SteamStartClient(u32),
    /// Ask Steam which compatibility tools one title may be run with, and step
    /// into the list.
    ///
    /// Carries the app because the list is fetched and the panel may be
    /// answered a moment later — a client that has to be started first is most
    /// of a minute — and by then "whatever is selected" is not what the press
    /// was about.
    SteamCompatibility(u32),
    /// Run it under one of them from now on, or under whatever Steam chooses,
    /// which is what `None` is.
    ///
    /// The tool is Valve's own key for it — `proton_experimental`, or the name
    /// a third-party tool declares itself with — rather than the label on the
    /// row, and it is borrowed for the life of the session because a command is
    /// a small copyable thing. See `settings::intern`, which is where the
    /// keyboard layouts do the same.
    SteamRunUnder {
        app_id: u32,
        tool: Option<&'static str>,
    },
    /// Do one thing to the Steam title the menu is about — check it, hand its
    /// install over — or bring up the Steam client itself.
    ///
    /// One command carrying which, rather than one command each, because
    /// unlike the display rows these are not three different journeys: every
    /// one of them is the same `steam:` URL handed to the same client, ends in
    /// a window of the client's own, and so needs sight given back first. What
    /// the row says is the only thing that differs. The title itself is not in
    /// the command, for the reason [`Command::Uninstall`] does not carry one:
    /// the menu is about whatever was selected when it was raised, and the
    /// shell can look that up again.
    SteamDo(lxb_steam::Doing),
    /// One answer to the question Valve's client stopped a launch on, by its
    /// place in the panel. The answers themselves live on the shell, because a
    /// command is a small copyable thing and one of these carries a sentence
    /// in whatever language Steam is running in.
    SteamLaunchAnswer(usize),
    /// Take an account off the machine, leaving what is in its home directory
    /// where it is — and the same, taking that with it.
    ///
    /// Two commands rather than one carrying a flag, for the reason
    /// [`Command::ConfirmUninstall`] is separate from [`Command::Uninstall`]:
    /// one of them removes an account and the other removes years of somebody's
    /// work, nothing in this shell can put the second back, and a single name
    /// for both would be one mis-routed press away from doing it. See
    /// `Shell::ask_about_removing_account`, which is the panel that offers them.
    RemoveAccountKeepingFiles,
    RemoveAccountAndFiles,
    /// Start the application that shelf is for — *Pictures*, over the row of
    /// photographs.
    ///
    /// The shelf is the user's own files and the row over it is the way into
    /// them; this is the way into the program that shows them properly, which
    /// has no tile of its own because a console does not want two rows called
    /// the same thing. See [`crate::apps::take_off_the_bar`] for why it is off
    /// the bar and still installed, and `Shell::shelf_entry_menu` for the row.
    OpenTheShelfApp,
    /// Put the menu away and do nothing else. The row that says so out loud,
    /// for a user who has opened the menu and changed their mind; `B` does the
    /// same thing and is not discoverable.
    Dismiss,
    /// A row that is here to prove the menu works and has nothing behind it
    /// yet. The name is what gets logged when it is chosen, so a placeholder
    /// can be told from its neighbours while the real commands are written.
    ///
    /// Every one of these is meant to be replaced by a variant of its own, and
    /// every one of them since has been: no menu the shell raises today carries
    /// one. It stays because that is how the next menu starts — and because the
    /// tests for this module and for the drawing need a command that stands for
    /// any command, which is exactly what it is.
    #[allow(dead_code)]
    Placeholder(&'static str),
}

/// One row of the menu.
///
/// Built rather than matched on: the entries a menu offers depend on what it
/// was raised over — which application, which window, whether there is a second
/// display to move to — so they are assembled at the moment the menu opens
/// instead of being a fixed list somewhere.
///
/// Not `Eq`, and it cannot be: a row can carry a level, and a level is where a
/// control stands rather than which of a set of things it is.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub command: Command,
    pub label: String,
    /// A second, quieter line under the label.
    ///
    /// For a row whose subject has more to it than a name — an announcement,
    /// which is a summary *and* whatever the program went on to say. Not a
    /// description of what the row does: every other row here is a command,
    /// and a command that needs explaining under itself is a command that is
    /// badly named.
    ///
    /// A row with one takes more of the column, exactly as a track does, and
    /// the same place decides both — see `ui::context_row_height`.
    pub detail: Option<String>,
    /// A short line *above* the label, quieter and smaller than either of the
    /// two under it: when an announcement arrived.
    ///
    /// Above rather than folded into the line below, which is where it used to
    /// be. Sharing a line with the body made the two run together into one
    /// sentence that begins with a time — "now · Would you like to install
    /// updates now?" — and reading it meant finding the separator first. On its
    /// own line the eye takes it in one glance and drops to the summary, and
    /// the summary starts where every other row's label starts.
    ///
    /// Above the label rather than below the body because it is not part of
    /// what the program said: it is the shell's note about *when*, and a note
    /// about a thing belongs before the thing rather than trailing off the end
    /// of it. It is also the one run every row of the list has, which is what
    /// makes a column of them scannable — the eye runs down the times.
    ///
    /// Never wraps and never grows the row beyond the one line it asks for:
    /// what goes here is "now", "3m", "2h" — see `age_of`.
    pub stamp: Option<String>,
    /// Drawn at the head of the row, if the shell has such a glyph.
    pub glyph: Option<&'static str>,
    /// A picture of the thing the row is about, by icon name, drawn instead of
    /// a glyph and larger — an application's own icon rather than one of the
    /// shell's symbols. Anything the icon theme cannot answer for falls back to
    /// the generic application icon, which is the honest picture of a program
    /// the machine knows nothing else about.
    pub icon: Option<String>,
    /// Where the row's own control stands, for a row that is a track rather
    /// than a command.
    ///
    /// A track is *driven* rather than pressed: Left and Right move it, and the
    /// press the row still has changes the one thing about a level that is not
    /// a position — whether it is silenced. So a row with one of these does not
    /// put the panel away when it is chosen; see [`Menu::choose`].
    pub level: Option<Level>,
    /// Whether it can be chosen at all. A row that cannot is drawn as an
    /// outline rather than a chip and the highlight steps straight over it —
    /// the same answer in both places, for the same reason the guide's tiles
    /// use one: a control drawn as available that the selection then refuses to
    /// stop on is worse than either failure on its own.
    pub enabled: bool,
    /// Whether it is not a control at all, but writing put on the panel to be
    /// read — an announcement's body, on the panel raised to show it.
    ///
    /// It cannot be chosen either, so it carries `enabled: false` with it, but
    /// the two are not the same thing and are drawn differently. A row that is
    /// unavailable is dimmed, because dim is how the shell says *not for you*.
    /// This one is the reason the panel is up, so it is set in the same white
    /// as everything else and stands at its full height from the moment the
    /// panel arrives — it can hardly wait for a highlight that will never come.
    pub reading: bool,
    /// A second command on the row, reached by pressing Right, drawn as its own
    /// small chip at the right-hand end.
    ///
    /// For the one thing a row is most often wanted for, put where it can be
    /// had without stepping into the row: an announcement is nearly always read
    /// and thrown away, and doing that through the row itself means opening it
    /// and finding Dismiss at the bottom of a list. One sideways press instead.
    ///
    /// Deliberately not a third row, and not a swipe: the highlight is the
    /// shell's one statement of where the user is standing, and a control it
    /// cannot stand on is a control nobody using a pad can reach.
    pub aside: Option<Aside>,
    /// Whether there is no coming back from choosing it, or from where it
    /// leads. The light that arrives on it is warm instead of the accent's, so
    /// the irreversible row is never picked by muscle memory alone.
    ///
    /// The light and nothing else: the label is the same white as every other
    /// row's, and so is the chip under it while the highlight is elsewhere. A
    /// warm label was tried and is exactly wrong here — it is a dim red laid on
    /// the panel's dark glass, so the row the user most needs to read is the one
    /// they cannot. A permanently red *chip* was tried too, on the Yes of a
    /// yes-or-no, and it is wrong for a quieter reason: a panel with one button
    /// already lit has answered its own question before the user has, and the
    /// highlight — the shell's one way of saying where you are standing — has
    /// nothing left to say when it arrives.
    ///
    /// This is the only mark of an irreversible choice anywhere in the shell:
    /// one flag, one warmth, on a menu row and on the answer to a question
    /// alike, so that a Steam game and a file and an application are not three
    /// different-looking ways of being asked the same thing.
    pub grave: bool,
    /// Which band of the menu it belongs to. A rule is drawn wherever two
    /// neighbouring rows disagree, exactly as the guide's column does it, so
    /// grouping is a number on an entry rather than a separator entry that
    /// navigation would then have to skip.
    pub group: u8,
    /// Whether choosing it leaves the panel standing.
    ///
    /// The ordinary row is a way *off* the menu: it is chosen, it is watched
    /// going down, and the panel folds back into the control it grew out of.
    /// This one is a control *on* the menu — it answers by changing something
    /// the user is still looking at, and a panel that folded away would take
    /// the answer with it before it could be read. A mixer's tracks are like
    /// that, and so is a list of alternatives where exactly one is in force:
    /// the tick moving to the row just pressed *is* the answer, and the user
    /// leaves when they have finished setting it rather than because the shell
    /// decided one press was enough.
    pub holds: bool,
    /// How many lines this row's label needs to be read in full, when it is
    /// given the room.
    ///
    /// One for every row the shell writes itself, because the shell names
    /// things to fit. It is the rows carrying somebody else's words that
    /// overflow — an announcement's summary is written by the program that
    /// sent it and can be a sentence — and those are the rows that grow when
    /// the highlight stops on them.
    ///
    /// Counted rather than guessed: only the renderer knows how wide a word
    /// is, so whoever builds the rows asks it once — see `Gpu::lines_needed`
    /// — and the answer travels here. It is a property of the row and not of
    /// the frame, which is why it is stored rather than measured while
    /// drawing.
    pub lines: u8,
    /// The same for the second line under the label — see [`Entry::detail`].
    ///
    /// Counted apart from the label because it is a different size and can be
    /// much the longer of the two: an announcement's summary is a headline and
    /// its body is the sentence. A row opens out by however many lines the two
    /// of them ask for between them.
    pub detail_lines: u8,
}

impl Entry {
    pub fn new(command: Command, label: impl Into<String>) -> Self {
        Self {
            command,
            label: label.into(),
            detail: None,
            stamp: None,
            glyph: None,
            icon: None,
            level: None,
            enabled: true,
            reading: false,
            aside: None,
            grave: false,
            group: 0,
            holds: false,
            lines: 1,
            detail_lines: 1,
        }
    }

    /// Give the row a second line — see [`Entry::detail`]. An empty one is
    /// no line at all, so a caller can pass whatever a program sent without
    /// having to ask first whether it sent anything.
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        self.detail = (!detail.trim().is_empty()).then_some(detail);
        self
    }

    /// Give the row a line above its label — see [`Entry::stamp`]. An empty
    /// one is no line at all, on the same terms the detail keeps.
    pub fn stamp(mut self, stamp: impl Into<String>) -> Self {
        let stamp = stamp.into();
        self.stamp = (!stamp.trim().is_empty()).then_some(stamp);
        self
    }

    pub fn glyph(mut self, glyph: &'static str) -> Self {
        self.glyph = Some(glyph);
        self
    }

    pub fn icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// Make the row a track standing at `level`. A track is driven rather than
    /// pressed, so it holds the panel by definition.
    pub fn level(mut self, level: Level) -> Self {
        self.level = Some(level);
        self.holds()
    }

    /// Leave the panel standing when this row is chosen — see [`Entry::holds`].
    pub fn holds(mut self) -> Self {
        self.holds = true;
        self
    }

    /// Offer the row, but greyed out — for something that is a command here in
    /// general and simply cannot be done from where the user is standing.
    /// A row that is *never* possible should be left out instead.
    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    /// Make the row writing to be read rather than a control — see
    /// [`Entry::reading`].
    pub fn reading(mut self) -> Self {
        self.enabled = false;
        self.reading = true;
        self
    }

    /// Hang a second command off the right-hand end of the row — see
    /// [`Entry::aside`].
    pub fn aside(mut self, command: Command, glyph: &'static str) -> Self {
        self.aside = Some(Aside { command, glyph });
        self
    }

    /// Mark the row as one there is no coming back from — see [`Entry::grave`].
    /// The yes of a question that destroys something wears this too: it is the
    /// same fact about a control, and the shell has one way of drawing it.
    pub fn grave(mut self) -> Self {
        self.grave = true;
        self
    }

    pub fn group(mut self, group: u8) -> Self {
        self.group = group;
        self
    }
}

/// The light under the chosen row: where it is, and how fast it is travelling.
///
/// One critically damped spring, the same one the guide's chip rides: it leans
/// into a move rather than leaving at full speed, and a second press part-way
/// carries the first one's momentum on instead of starting again from rest. The
/// first frame snaps, so the glide is only ever between two real positions
/// rather than in from nowhere.
///
/// Its own type because two things in this shell carry one — a menu's rows, and
/// the rows of the panel an application's file question is answered in — and two
/// springs written out twice would be two places for the rate to drift apart.
#[derive(Debug, Default, Clone, Copy)]
pub struct Glide {
    at: Option<[f32; 4]>,
    speed: [f32; 4],
}

impl Glide {
    /// One frame of the glide towards `target`.
    pub fn towards(&mut self, target: [f32; 4], dt: f32) -> [f32; 4] {
        let Some(current) = self.at else {
            self.speed = [0.0; 4];
            self.at = Some(target);
            return target;
        };
        let mut next = [0.0; 4];
        for ((slot, velocity), (from, to)) in next
            .iter_mut()
            .zip(self.speed.iter_mut())
            .zip(current.iter().zip(&target))
        {
            let (at, moving) = lxb_protocol::overview::spring(
                *from as f64,
                *velocity as f64,
                *to as f64,
                HIGHLIGHT_EASE_RATE as f64,
                dt as f64,
            );
            (*slot, *velocity) = (at as f32, moving as f32);
        }
        self.at = Some(next);
        next
    }
}

/// The header at the top of a panel: what the menu is about, and how many
/// lines saying it takes.
///
/// The line count is here rather than worked out while drawing for the same
/// reason a row's is — see [`Entry::lines`]: how wide a word is is known only
/// to the thing that shapes text, and the panel has to be built to hold the
/// header before anything is drawn on it. One line unless somebody who can
/// measure says otherwise, which is what leaves every panel in the shell
/// exactly as it was: their headers are names the shell chose to fit.
///
/// And it travels *with* the title rather than being set on the menu after the
/// fact, because a list stepped into arrives a press later than it is asked
/// for — see [`Menu::descend`]. A line count that landed on the panel ahead of
/// the words it was measured for would open the header the list on screen has
/// no use for, and the panel would jump before the step.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Title {
    pub text: String,
    pub lines: u8,
}

impl Title {
    /// A header set on one line, which is every header the shell writes itself.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            lines: 1,
        }
    }

    /// The same, opened out to hold a heading somebody else wrote.
    pub fn lines(mut self, lines: u8) -> Self {
        self.lines = lines.max(1);
        self
    }
}

impl From<String> for Title {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for Title {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

/// The button on the right-hand end of a row — see [`Entry::aside`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aside {
    pub command: Command,
    /// One of the shell's own marks, never an application's picture: this is a
    /// button that does one named thing, and it is the same button on every row
    /// that has it.
    pub glyph: &'static str,
}

/// How long the panel takes to grow out of its anchor, and to fall back into
/// it. The same in both directions: it is one journey, and a menu that left
/// faster than it arrived reads as having been dropped rather than put away.
pub const FLIGHT: f32 = 0.2;

/// How long a row takes to open out to its full label, and to close again.
///
/// Slower than the panel's own flight, and deliberately. The panel arriving is
/// the answer to a press and wants to be quick; a row unfolding is the shell
/// showing something that was already there, under a highlight that has just
/// come to rest. At the panel's speed it reads as a jolt in a column that had
/// finished moving.
const UNFOLD: f32 = 0.32;

/// How long a chosen row is watched going down before the panel starts folding
/// away. Short — a menu row is a button being pressed, not a switch being
/// thrown, so it wants the dip and not the latch.
pub const PRESS_TIME: f32 = 0.16;

/// How stiff the spring the highlight glides on is, in radians per second.
/// The guide's, so the two surfaces feel related.
pub const HIGHLIGHT_EASE_RATE: f32 = 21.0;

/// The menu as the shell holds it.
///
/// One per session rather than one per surface: only one can be up at a time
/// by definition — it takes every button while it is — so a second would be a
/// second answer to the question of who has the keys.
#[derive(Debug, Default)]
pub struct Menu {
    /// Whether it is taking input. Not the same question as whether it is on
    /// screen: a menu that has been answered hands the keys straight back while
    /// the panel is still folding into its anchor.
    open: bool,
    entries: Vec<Entry>,
    title: Option<Title>,
    /// The control it was raised over, in display coordinates.
    anchor: [f32; 4],
    selected: usize,
    /// Whether the highlight is standing on the selected row's button rather
    /// than on the row itself — see [`Entry::aside`].
    ///
    /// A second coordinate and not a second selection: the row underneath is
    /// still the selected one, still opened out, still the thing the panel is
    /// talking about. All this says is which of the row's two commands the
    /// next press carries out.
    on_aside: bool,
    /// First row drawn, for a list longer than the display holds.
    scroll: usize,
    /// How much wider than usual the panel is drawn, in reference pixels.
    ///
    /// Held by the menu rather than decided while drawing because it belongs to
    /// the whole session at the panel — a list stepped into from a wide one
    /// stays wide, or the panel would change size under the user on the way in.
    extra_width: f32,
    /// How many rows that is. Told to the menu by the shell, which is the only
    /// half of this that knows how big the screen is; zero means nobody has
    /// said yet, which is treated as "they all fit".
    window: usize,
    /// How far the panel is out of its anchor: 0 shut, 1 fully open.
    ///
    /// A position rather than the moment it opened, because it has to outlive
    /// the menu — see `open`. It is also what lets the two directions be one
    /// movement: a menu dismissed halfway out grows no further first.
    linear: f32,
    /// Eased rectangle of the selected row's chip, which slides down the
    /// column rather than jumping from row to row, and how fast it is going.
    highlight: Option<[f32; 4]>,
    highlight_speed: [f32; 4],
    /// How far the selected row has opened out to show a label too long for
    /// one line: 0 closed, 1 fully open.
    ///
    /// A position rather than a flag, for the reason [`Self::linear`] is one —
    /// it has to be able to travel back. Moving the highlight off a row that
    /// had opened does not shut it instantly; it closes on the way out while
    /// the next one opens, which is what stops the column jumping by the
    /// height of a line at the moment a key is pressed.
    expansion: f32,
    /// The row being pressed and when the press started, and whether the panel
    /// is to fold away once it has finished. A press outlives the keystroke:
    /// the row has to be *seen* to go down, which takes longer than the frame
    /// the button went down on.
    pressed: Option<(usize, Instant)>,
    /// Whether what is going down is the row's button rather than the row. Only
    /// one of the two ever sinks: they are side by side, and both moving at
    /// once would read as the whole line having been pressed.
    pressed_aside: bool,
    closing_after_press: bool,
    /// The list a row has asked for, waiting for that row to finish going down
    /// — see [`Menu::descend`].
    next: Option<(Option<Title>, Vec<Entry>)>,
    /// The lists this one was reached through, innermost last, so [`Menu::back`]
    /// can put one of them back.
    stack: Vec<Step>,
}

/// A list the menu has stepped out of, held so it can be stepped back into.
///
/// The scroll is in here as well as the selection, because coming back to a
/// long list at the top of it and not where it was left is the same failure as
/// coming back to it with the wrong row highlighted.
#[derive(Debug)]
struct Step {
    title: Option<Title>,
    entries: Vec<Entry>,
    selected: usize,
    on_aside: bool,
    scroll: usize,
}

impl Menu {
    // -- what is in it ------------------------------------------------------

    /// Raise the menu over `anchor` with these entries, on a display with room
    /// for `rows` of them. Returns whether it opened.
    ///
    /// A menu with nothing choosable in it does not open at all. A panel that
    /// grew out of a tile to offer four rows the highlight refuses to stop on
    /// is a dead end the user then has to press B to get out of, and the
    /// button that raised it is better read as having done nothing.
    pub fn open_at(
        &mut self,
        anchor: [f32; 4],
        title: Option<Title>,
        entries: Vec<Entry>,
        rows: usize,
    ) -> bool {
        self.open_selecting(anchor, title, entries, rows, 0)
    }

    /// The same, opening on the first choosable row at or after `from` instead
    /// of at the top of the list.
    ///
    /// For a question whose safe answer is not its first one. A confirmation
    /// opens on No, so that a user who answers it the way they answer
    /// everything else — by pressing accept the moment something appears — has
    /// declined rather than agreed.
    pub fn open_selecting(
        &mut self,
        anchor: [f32; 4],
        title: Option<Title>,
        entries: Vec<Entry>,
        rows: usize,
        from: usize,
    ) -> bool {
        if !entries.iter().any(|entry| entry.enabled) {
            return false;
        }
        self.window = rows;
        self.selected = entries
            .iter()
            .enumerate()
            .skip(from)
            .chain(entries.iter().enumerate())
            .find(|(_, entry)| entry.enabled)
            .map(|(index, _)| index)
            .unwrap_or_default();
        self.entries = entries;
        self.title = title;
        self.anchor = anchor;
        self.open = true;
        self.on_aside = false;
        self.extra_width = 0.0;
        self.scroll = 0;
        self.pressed = None;
        self.closing_after_press = false;
        self.next = None;
        self.stack.clear();
        self.highlight = None;
        self.highlight_speed = [0.0; 4];
        self.keep_selection_in_view();
        true
    }

    /// Raise it with nothing in it at all.
    ///
    /// The one case a column of commands has no commands: a panel the shell is
    /// busy behind, where the honest set of things the user can usefully press
    /// is empty. It still takes the keys — that is what makes it modal — and it
    /// answers every one of them with nothing, which is what
    /// [`Self::move_selection`] and [`Self::choose`] already do for an empty
    /// list.
    ///
    /// Deliberately its own method rather than [`Self::open_at`] relaxing its
    /// guard. That guard exists because a menu whose rows can none of them be
    /// chosen is a dead end the user has to press B to escape, and it should go
    /// on refusing to open. This is not that: it is a panel that has not
    /// finished, and something else is on its way to replace it.
    pub fn open_waiting(&mut self, anchor: [f32; 4]) {
        self.window = 0;
        self.selected = 0;
        self.entries = Vec::new();
        self.title = None;
        self.anchor = anchor;
        self.open = true;
        self.on_aside = false;
        self.extra_width = 0.0;
        self.scroll = 0;
        self.pressed = None;
        self.closing_after_press = false;
        self.next = None;
        self.stack.clear();
        self.highlight = None;
        self.highlight_speed = [0.0; 4];
    }

    // -- one list leading to another ---------------------------------------

    /// Step into a further list, out of the row that was just chosen.
    ///
    /// The panel stays exactly where it is and keeps the keys; what changes is
    /// what is written on it, and not until the row that asked for it has been
    /// seen to go down. That wait is the whole of why this is not simply
    /// [`Self::open_at`] a second time. A menu that swapped its rows on the
    /// frame of the press would take the pressed row's label away underneath
    /// the press, and one that folded into its anchor and grew back out would
    /// spend two thirds of a second saying nothing — for a step the user reads
    /// as going one level deeper into the same panel, which is what it is.
    ///
    /// Returns whether there was anything to step into. A list with nothing
    /// choosable in it is refused here for the same reason [`Self::open_at`]
    /// refuses one: it is a dead end with no way out but Back.
    pub fn descend(&mut self, title: Option<Title>, entries: Vec<Entry>) -> bool {
        if !entries.iter().any(|entry| entry.enabled) {
            return false;
        }
        self.next = Some((title, entries));
        // Choosing a row handed the keys back. The step is not a way *off* the
        // panel, so it takes them again — and cancels the fold that was about
        // to start.
        self.open = true;
        self.closing_after_press = false;
        true
    }

    /// Write a different list on the panel, in place of the one it is showing.
    ///
    /// For the one list whose rows are not known when it is opened: a panel
    /// that stepped into "asking Steam…" and now has the answer. Not
    /// [`Self::descend`], which would put a second list on the stack and leave
    /// Back stepping through a wait nobody wants to see again, and not
    /// [`Self::open_at`], which would fold the panel away and grow it back for
    /// what the user reads as one list arriving.
    ///
    /// A list arriving while the step into it is still in flight replaces what
    /// is arriving rather than what is on the screen — otherwise the answer
    /// would be written over the *menu the step came from*, and the wait would
    /// then land on top of it.
    ///
    /// Returns whether there was anything to write, on the terms
    /// [`Self::descend`] returns them: a list with nothing choosable in it is a
    /// dead end, and the caller keeps whatever it has.
    pub fn replace(&mut self, title: Option<Title>, entries: Vec<Entry>) -> bool {
        if !entries.iter().any(|entry| entry.enabled) {
            return false;
        }
        if self.next.is_some() {
            self.next = Some((title, entries));
            return true;
        }
        self.title = title;
        self.entries = entries;
        // Onto the first row that can be chosen, wherever the wait had left it.
        // Not kept where it was: the rows underneath it are different rows now,
        // and a selection that stayed on index 1 would be standing on whichever
        // tool happened to arrive there.
        self.selected = self
            .entries
            .iter()
            .position(|entry| entry.enabled)
            .unwrap_or_default();
        self.on_aside = false;
        self.scroll = 0;
        self.pressed = None;
        self.closing_after_press = false;
        self.keep_selection_in_view();
        true
    }

    /// Whether a further list is on its way in, so a caller that is about to do
    /// something else with the panel knows one has been asked for.
    pub fn is_descending(&self) -> bool {
        self.next.is_some()
    }

    /// Step back out to the list this one was reached from. `false` when there
    /// is none, which is what tells the caller that Back means closing the
    /// whole panel.
    ///
    /// The highlight is deliberately not reset: it glides from wherever it is
    /// to the row being returned to, exactly as it would between two rows of
    /// one list. Coming back is a movement, not a new panel.
    pub fn back(&mut self) -> bool {
        // A list that has been asked for but has not arrived is simply
        // forgotten. The user has changed their mind inside the press.
        if self.next.take().is_some() {
            return true;
        }
        let Some(step) = self.stack.pop() else {
            return false;
        };
        self.title = step.title;
        self.entries = step.entries;
        self.selected = step.selected;
        self.on_aside = step.on_aside;
        self.scroll = step.scroll;
        self.pressed = None;
        self.closing_after_press = false;
        self.keep_selection_in_view();
        true
    }

    /// How deep in it is: zero on the list it was opened with.
    #[cfg(test)]
    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Put the waiting list on the panel, keeping the one it replaces.
    fn enter(&mut self, title: Option<Title>, entries: Vec<Entry>) {
        self.stack.push(Step {
            title: std::mem::replace(&mut self.title, title),
            entries: std::mem::replace(&mut self.entries, entries),
            selected: self.selected,
            on_aside: self.on_aside,
            scroll: self.scroll,
        });
        self.selected = self
            .entries
            .iter()
            .position(|entry| entry.enabled)
            .unwrap_or_default();
        self.on_aside = false;
        self.scroll = 0;
        self.keep_selection_in_view();
    }

    /// Put it away. Returns whether it was open, so a caller can tell a
    /// dismissal from a press that has to go on to mean something else.
    ///
    /// The panel is not gone afterwards — it is falling back into its anchor,
    /// and [`Self::is_on_screen`] stays true until it has arrived.
    pub fn close(&mut self) -> bool {
        let was = self.open;
        self.open = false;
        self.closing_after_press = false;
        // Whatever it was in the middle of going into, it is not going there:
        // the panel folding into its anchor is the end of the whole journey,
        // not of the innermost list.
        self.next = None;
        self.stack.clear();
        was
    }

    /// Whether it is taking input.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether there is anything of it left to draw.
    pub fn is_on_screen(&self) -> bool {
        self.open || self.linear > 0.0 || self.pressed.is_some()
    }

    /// Whether something is still moving, so the display it is on keeps
    /// drawing frames until it has settled.
    pub fn is_animating(&self) -> bool {
        (self.linear > 0.0 && self.linear < 1.0) || self.pressed.is_some()
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn title(&self) -> Option<&str> {
        self.title.as_ref().map(|title| title.text.as_str())
    }

    /// How many lines the header takes — see [`Title`]. One where there is no
    /// header at all, so a caller can multiply by it without asking twice.
    pub fn title_lines(&self) -> u8 {
        self.title.as_ref().map_or(1, |title| title.lines.max(1))
    }

    pub fn anchor(&self) -> [f32; 4] {
        self.anchor
    }

    pub fn selected(&self) -> usize {
        self.selected.min(self.entries.len().saturating_sub(1))
    }

    // -- how much of it is on screen ---------------------------------------

    /// Say how many rows the display it is on has room for, after the fact —
    /// for a screen that changed size with the menu already up.
    ///
    /// Ordinarily this comes in with [`Self::open_at`]. It is held rather than
    /// passed in each time because the *scroll* is state: what the user can see
    /// has to survive between one press of Down and the next, and the only
    /// thing that decides it is how many rows fit.
    pub fn set_window(&mut self, rows: usize) {
        if rows == 0 || rows == self.window {
            return;
        }
        self.window = rows;
        self.keep_selection_in_view();
    }

    /// How many rows are drawn: every one of them, unless the screen is too
    /// short to hold them.
    pub fn visible_rows(&self) -> usize {
        match self.window {
            0 => self.entries.len(),
            window => window.min(self.entries.len()),
        }
    }

    /// The first row drawn.
    pub fn first_visible(&self) -> usize {
        self.scroll
            .min(self.entries.len().saturating_sub(self.visible_rows()))
    }

    /// Whether the list carries on above or below what is drawn, which is what
    /// the little arrows at the ends of the column are for.
    pub fn scrolled_above(&self) -> bool {
        self.first_visible() > 0
    }

    pub fn scrolled_below(&self) -> bool {
        self.first_visible() + self.visible_rows() < self.entries.len()
    }

    /// Follow the selection with the window, moving it as little as it takes.
    fn keep_selection_in_view(&mut self) {
        let rows = self.visible_rows();
        if rows == 0 {
            return;
        }
        let selected = self.selected();
        self.scroll = self
            .scroll
            .min(selected)
            .max((selected + 1).saturating_sub(rows))
            .min(self.entries.len().saturating_sub(rows));
    }

    // -- driving it ---------------------------------------------------------

    /// Move the highlight up or down. Returns whether it actually moved.
    ///
    /// Wraps, and skips whatever cannot be chosen. A context menu is short by
    /// nature — it is the things that can be done to one object — so running
    /// off the end is more annoying than surprising, and a disabled row is one
    /// the user has already been told, by its outline, is not for them.
    pub fn move_selection(&mut self, delta: i32) -> bool {
        let count = self.entries.len();
        if count == 0 {
            return false;
        }
        let current = self.selected();
        let mut next = current;
        for _ in 0..count {
            next = (next as i32 + delta).rem_euclid(count as i32) as usize;
            if self.entries[next].enabled {
                break;
            }
        }
        self.selected = next;
        // The button carries down the column with the highlight, where the row
        // arrived at has one. A user throwing away three announcements in a row
        // should not have to step sideways again for each of them — and where
        // the next row has no button there is nothing to stand on, so the
        // highlight comes back to the row.
        self.on_aside = self.on_aside && self.entries[next].aside.is_some();
        self.keep_selection_in_view();
        next != current
    }

    /// Step sideways, onto the selected row's button or back off it. Returns
    /// whether the highlight actually moved.
    ///
    /// Only where there is a button: on every other row Left and Right go on
    /// meaning what they meant, which on a track is the track and on a plain
    /// command is nothing at all.
    pub fn move_aside(&mut self, delta: i32) -> bool {
        let has = self
            .selected_entry()
            .is_some_and(|entry| entry.aside.is_some() && entry.enabled);
        let want = delta > 0;
        if !has || self.on_aside == want {
            return false;
        }
        self.on_aside = want;
        true
    }

    /// Whether the highlight is on the selected row's button rather than on the
    /// row — see [`Entry::aside`].
    pub fn on_aside(&self) -> bool {
        self.on_aside
    }

    /// Put the highlight straight on row `index`.
    ///
    /// What a pointer does: the panel stands still, so the row under the cursor
    /// simply is the selected row. A row that cannot be chosen is left alone
    /// rather than landed on — its outline has already said as much, and moving
    /// the highlight onto it would leave the user with a selection that answers
    /// nothing.
    ///
    /// The window is not moved. A row being pointed at is by definition a row
    /// already on screen, and scrolling to it would take it out from under the
    /// cursor that arrived on it.
    pub fn select(&mut self, index: usize) -> bool {
        if !self.entries.get(index).is_some_and(|entry| entry.enabled) {
            return false;
        }
        if self.selected() == index && !self.on_aside {
            return false;
        }
        self.selected = index;
        // A pointer lands on whatever it is over, and a row is not its button:
        // the button has a rectangle of its own to be pointed at — see
        // [`Self::select_aside`].
        self.on_aside = false;
        true
    }

    /// The same for a row's button, which is pointed at in its own right.
    pub fn select_aside(&mut self, index: usize) -> bool {
        if !self
            .entries
            .get(index)
            .is_some_and(|entry| entry.enabled && entry.aside.is_some())
        {
            return false;
        }
        if self.selected() == index && self.on_aside {
            return false;
        }
        self.selected = index;
        self.on_aside = true;
        true
    }

    /// Choose the highlighted row: start its press, hand the keys back, and
    /// return what was asked for.
    ///
    /// The panel does not go anywhere yet. It stays exactly where it is until
    /// the row has been seen to go down, and only then folds into its anchor —
    /// so what the user watches is the button they pressed answering them,
    /// rather than a menu that vanished at the moment of the press.
    ///
    /// A row that holds is the exception, and keeps the keys: it is a control
    /// *on* the panel rather than a way off it, its answer is the panel itself
    /// changing, and one that folded away would take that answer with it
    /// before it could be seen. See [`Entry::holds`].
    /// A press on the row's button is the same movement against the other of
    /// its two commands — and it always holds the panel. The button is a
    /// control *on* the list: throwing an announcement away is answered by the
    /// row leaving, and a panel that folded up on the first one would make
    /// clearing three of them three journeys.
    pub fn choose(&mut self) -> Option<Command> {
        let index = self.selected();
        let entry = self.entries.get(index).filter(|entry| entry.enabled)?;
        let (command, holds) = match entry.aside.filter(|_| self.on_aside) {
            Some(aside) => (aside.command, true),
            None => (entry.command, entry.holds),
        };
        self.pressed = Some((index, Instant::now()));
        self.pressed_aside = self.on_aside;
        self.closing_after_press = !holds;
        self.open = holds;
        Some(command)
    }

    /// The row the highlight is on, for a caller that has to know more about it
    /// than which command it carries.
    pub fn selected_entry(&self) -> Option<&Entry> {
        self.entries.get(self.selected())
    }

    /// Draw the panel wider than usual by `extra` reference pixels — see
    /// [`Self::extra_width`].
    ///
    /// Asked of the menu after it is raised rather than passed in with the
    /// entries, because it is the one thing about the panel that is a matter
    /// of what is written on it rather than of what can be done with it, and
    /// every other caller wants the usual width.
    pub fn widen(&mut self, extra: f32) {
        self.extra_width = extra.max(0.0);
    }

    /// How much wider than usual it is being drawn, in reference pixels.
    pub fn extra_width(&self) -> f32 {
        self.extra_width
    }

    /// Put a fresh set of rows on an open panel.
    ///
    /// For a menu that is about something still moving — the mixer, whose rows
    /// are what the machine is playing right now. Two different things can have
    /// happened since the last look, and they are told apart by the commands
    /// alone:
    ///
    /// * the same rows standing somewhere else, which is a level being read
    ///   back or a slider the user has just moved. Nothing about the panel may
    ///   move: the highlight stays where it is, mid-glide if it is gliding, and
    ///   a long list stays scrolled where the user left it.
    /// * a different set of rows, because an application started or stopped
    ///   making a noise. Then the highlight follows the row it was on, if that
    ///   row is still there, and otherwise stays where it was standing — the
    ///   nearest choosable row at or after the place it had got to, and only
    ///   then one before it.
    ///
    /// That last rule is not the obvious one. Falling back to the top of the
    /// list is simpler and was what this did, and it is wrong for the same
    /// reason a list that renumbered itself under the user's hand would be:
    /// pressing a row that removes itself — dismissing an announcement — is
    /// the ordinary way to work down a list, and a highlight that jumped to
    /// row one after every press would make the second press land somewhere
    /// nobody aimed. In the notification panel row one is Clear All.
    ///
    /// Reports whether anything at all changed, so the caller can leave the
    /// display alone when nothing did.
    /// Its own call for the header, which [`Menu::refresh`] leaves alone: the
    /// one panel whose header changes while it is up is the one over a column
    /// being marked, where the header is the count — see
    /// `Shell::refresh_the_marking_menu`. Everywhere else the header names the
    /// thing the menu was raised over, which cannot change while the menu is
    /// standing over it, so a refresh that rewrote it would be a call the
    /// caller has to remember to pass the same string to every time.
    pub fn retitle(&mut self, title: Title) -> bool {
        if self.title.as_ref() == Some(&title) {
            return false;
        }
        self.title = Some(title);
        true
    }

    pub fn refresh(&mut self, entries: Vec<Entry>) -> bool {
        if entries == self.entries {
            return false;
        }
        let same_rows = entries.len() == self.entries.len()
            && entries
                .iter()
                .zip(&self.entries)
                .all(|(fresh, held)| fresh.command == held.command);
        if same_rows {
            self.entries = entries;
            return true;
        }

        let was = self.selected_entry().map(|entry| entry.command);
        let stood = self.selected();
        self.entries = entries;
        let choosable = |from: usize, to: usize| {
            (from..to.min(self.entries.len())).find(|index| self.entries[*index].enabled)
        };
        self.selected = was
            .and_then(|command| {
                self.entries
                    .iter()
                    .position(|entry| entry.command == command && entry.enabled)
            })
            // Where it was standing, or the first row after it — what closing a
            // gap in a list looks like when the row that closed it is the one
            // the user just pressed.
            .or_else(|| choosable(stood, self.entries.len()))
            // And only then backwards, for the row that was last in its list.
            .or_else(|| {
                (0..stood.min(self.entries.len())).rfind(|index| self.entries[*index].enabled)
            })
            .unwrap_or_default();
        // The button carries over only where the row arrived at has one — the
        // same rule the highlight follows down the column.
        self.on_aside = self.on_aside
            && self
                .entries
                .get(self.selected)
                .is_some_and(|entry| entry.aside.is_some());
        // A row that has gone takes its press with it: what was being watched
        // going down is not on the panel any more.
        if self
            .pressed
            .is_some_and(|(index, _)| index != self.selected)
        {
            self.pressed = None;
        }
        self.keep_selection_in_view();
        true
    }

    /// How far through its press row `index` is, 0 at the row going down and 1
    /// once it is back. `None` when it is not being pressed, which is every row
    /// on almost every frame.
    pub fn press_progress(&self, index: usize) -> Option<f32> {
        self.press_at(index, false)
    }

    /// The same for the button on the right-hand end of row `index`.
    pub fn aside_press_progress(&self, index: usize) -> Option<f32> {
        self.press_at(index, true)
    }

    fn press_at(&self, index: usize, aside: bool) -> Option<f32> {
        let (pressed, at) = self.pressed?;
        if pressed != index || self.pressed_aside != aside {
            return None;
        }
        let progress = at.elapsed().as_secs_f32() / PRESS_TIME;
        (progress < 1.0).then_some(progress)
    }

    // -- the movement -------------------------------------------------------

    /// Advance the panel's growth by `dt` and return where it is now, 0 shut
    /// and 1 open.
    ///
    /// One number in both directions, so a menu dismissed before it finished
    /// opening falls back from where it is rather than snapping open first.
    pub fn animate(&mut self, dt: f32) -> f32 {
        // A press that has finished being watched is what releases the panel.
        // Until then the menu holds its ground: nothing here disappears before
        // its own transition has ended.
        if self
            .pressed
            .is_some_and(|(index, _)| self.press_progress(index).is_none())
        {
            self.pressed = None;
            self.closing_after_press = false;
        }
        // And a press that has finished is also what lets a further list on to
        // the panel — for the same reason and in the same breath. The row is
        // watched all the way down, and then the list it asked for arrives.
        if self.pressed.is_none() {
            if let Some((title, entries)) = self.next.take() {
                self.enter(title, entries);
            }
        }
        // The selected row opening out to show the rest of its label, or
        // closing again behind the highlight as it leaves.
        //
        // Only while the panel itself is all the way out. A row unfolding
        // during the panel's own flight is two movements at once out of one
        // press, and the one that matters is the panel arriving.
        let opening = self.open
            && self.linear >= 1.0
            && self
                .selected_entry()
                .is_some_and(|entry| (entry.lines > 1 || entry.detail_lines > 1) && entry.enabled);
        let unfold = dt / UNFOLD;
        self.expansion = if opening {
            (self.expansion + unfold).min(1.0)
        } else {
            (self.expansion - unfold).max(0.0)
        };

        let held_open = self.open || self.closing_after_press;
        let target = if held_open { 1.0 } else { 0.0 };
        let step = dt / FLIGHT;
        self.linear = if self.linear < target {
            (self.linear + step).min(target)
        } else {
            (self.linear - step).max(target)
        };
        self.linear
    }

    /// How far the selected row has opened out, eased — 0 for a panel where
    /// nothing is opening, which is nearly all of them.
    pub fn expansion(&self) -> f32 {
        crate::ui::ease(self.expansion)
    }

    /// One frame of the highlight's glide towards `target`. See [`Glide`],
    /// which is the spring and the whole of the rule.
    pub fn animate_highlight(&mut self, target: [f32; 4], dt: f32) -> [f32; 4] {
        let mut glide = Glide {
            at: self.highlight,
            speed: self.highlight_speed,
        };
        let next = glide.towards(target, dt);
        self.highlight = glide.at;
        self.highlight_speed = glide.speed;
        next
    }

    /// Pretend the press started `seconds` ago, so tests can assert on the
    /// middle of the movement instead of racing it.
    #[cfg(test)]
    pub fn backdate_press(&mut self, seconds: f32) {
        if let Some((_, at)) = self.pressed.as_mut() {
            *at = at
                .checked_sub(std::time::Duration::from_secs_f32(seconds))
                .unwrap_or(*at);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(labels: &[&'static str]) -> Vec<Entry> {
        labels
            .iter()
            .map(|label| Entry::new(Command::Placeholder(label), *label))
            .collect()
    }

    /// A menu whose display has room for every row it was given.
    fn open(labels: &[&'static str]) -> Menu {
        let count = labels.len();
        let mut menu = Menu::default();
        assert!(menu.open_at([10.0, 10.0, 40.0, 40.0], None, rows(labels), count));
        menu
    }

    #[test]
    fn a_menu_with_nothing_choosable_never_opens() {
        let mut menu = Menu::default();
        let dead: Vec<Entry> = rows(&["one", "two"])
            .into_iter()
            .map(Entry::disabled)
            .collect();
        assert!(!menu.open_at([0.0; 4], None, dead, 8));
        assert!(!menu.is_open());
        assert!(!menu.is_on_screen());
        assert!(!menu.open_at([0.0; 4], None, Vec::new(), 8));
    }

    #[test]
    fn it_opens_on_the_first_row_that_can_be_chosen() {
        let mut menu = Menu::default();
        let mut entries = rows(&["one", "two", "three"]);
        entries[0] = entries[0].clone().disabled();
        assert!(menu.open_at([0.0; 4], None, entries, 8));
        assert_eq!(menu.selected(), 1);
    }

    /// A pointer names the row it is on, and a row it may not choose refuses
    /// the highlight: a click on one must not press whatever was selected
    /// before, and the outline has already said it is not for pressing.
    #[test]
    fn a_row_can_be_pointed_at_and_a_disabled_one_refuses() {
        let mut menu = Menu::default();
        let mut entries = rows(&["one", "two", "three"]);
        entries[1] = entries[1].clone().disabled();
        menu.open_at([0.0; 4], None, entries, 8);

        assert!(menu.select(2));
        assert_eq!(menu.selected(), 2);
        // The row it is already on is not a move.
        assert!(!menu.select(2));

        assert!(!menu.select(1), "a disabled row is not selected by a click");
        assert_eq!(menu.selected(), 2);
        assert!(!menu.select(9));
    }

    #[test]
    fn the_highlight_steps_over_rows_it_cannot_stop_on() {
        let mut menu = Menu::default();
        let mut entries = rows(&["one", "two", "three"]);
        entries[1] = entries[1].clone().disabled();
        menu.open_at([0.0; 4], None, entries, 8);

        assert!(menu.move_selection(1));
        assert_eq!(menu.selected(), 2, "the disabled middle row was landed on");
        // And wraps, because the list is short by nature.
        assert!(menu.move_selection(1));
        assert_eq!(menu.selected(), 0);
        assert!(menu.move_selection(-1));
        assert_eq!(menu.selected(), 2);
    }

    #[test]
    fn a_single_choosable_row_reports_that_it_did_not_move() {
        let mut menu = Menu::default();
        let mut entries = rows(&["one", "two"]);
        entries[1] = entries[1].clone().disabled();
        menu.open_at([0.0; 4], None, entries, 8);
        assert!(!menu.move_selection(1));
        assert_eq!(menu.selected(), 0);
    }

    /// The window follows the selection by as little as it takes, in both
    /// directions, and never runs off either end of the list.
    #[test]
    fn a_long_list_scrolls_under_a_fixed_window() {
        let mut menu = open(&["a", "b", "c", "d", "e", "f", "g"]);
        menu.set_window(3);
        assert_eq!(menu.visible_rows(), 3);
        assert_eq!(menu.first_visible(), 0);
        assert!(!menu.scrolled_above() && menu.scrolled_below());

        for _ in 0..2 {
            menu.move_selection(1);
        }
        assert_eq!((menu.selected(), menu.first_visible()), (2, 0));
        menu.move_selection(1);
        assert_eq!(
            (menu.selected(), menu.first_visible()),
            (3, 1),
            "the window moved further than the one row it had to"
        );
        assert!(menu.scrolled_above() && menu.scrolled_below());

        // Wrapping off the end takes the window with it, to the top.
        for _ in 0..4 {
            menu.move_selection(1);
        }
        assert_eq!((menu.selected(), menu.first_visible()), (0, 0));

        // And off the top to the bottom, where the window stops with the last
        // row against the foot of the panel rather than past it.
        menu.move_selection(-1);
        assert_eq!((menu.selected(), menu.first_visible()), (6, 4));
        assert!(menu.scrolled_above() && !menu.scrolled_below());
    }

    #[test]
    fn a_list_that_fits_never_scrolls() {
        let mut menu = open(&["a", "b"]);
        menu.set_window(6);
        assert_eq!(menu.visible_rows(), 2);
        assert!(!menu.scrolled_above() && !menu.scrolled_below());
        menu.move_selection(1);
        assert_eq!(menu.first_visible(), 0);
    }

    /// Choosing hands the keys back at once, but nothing disappears before its
    /// transition has finished: the panel is still on screen, and still fully
    /// open, for as long as the row is being watched going down.
    #[test]
    fn a_chosen_row_is_watched_before_the_panel_folds_away() {
        let mut menu = open(&["one", "two"]);
        while menu.animate(0.05) < 1.0 {}

        assert_eq!(menu.choose(), Some(Command::Placeholder("one")));
        assert!(!menu.is_open(), "it should hand the keys back at once");
        assert!(menu.is_on_screen());
        assert!(menu.press_progress(0).is_some());
        assert!(menu.press_progress(1).is_none());

        // Half way through the press, the panel has not begun to leave.
        menu.backdate_press(PRESS_TIME * 0.5);
        assert_eq!(menu.animate(1.0 / 60.0), 1.0);

        // Once it has been seen, and only then, the panel folds into its
        // anchor — and stops being drawn when it gets there.
        menu.backdate_press(PRESS_TIME);
        assert!(menu.animate(1.0 / 60.0) < 1.0);
        assert!(menu.press_progress(0).is_none());
        while menu.animate(0.05) > 0.0 {}
        assert!(!menu.is_on_screen());
    }

    #[test]
    fn a_disabled_row_cannot_be_chosen() {
        let mut menu = Menu::default();
        let mut entries = rows(&["one", "two"]);
        entries[1] = entries[1].clone().disabled();
        menu.open_at([0.0; 4], None, entries, 8);
        menu.move_selection(1);
        assert_eq!(menu.selected(), 0, "the highlight never reached it");
        assert!(menu.choose().is_some());
    }

    /// Dismissed half way out, it falls back from where it is rather than
    /// finishing its arrival first.
    #[test]
    fn dismissing_it_reverses_the_growth_from_where_it_is() {
        let mut menu = open(&["one"]);
        let half = menu.animate(FLIGHT * 0.5);
        assert!((0.0..1.0).contains(&half));
        assert!(menu.close());
        let after = menu.animate(1.0 / 60.0);
        assert!(after < half, "{after} should be below {half}");
        assert!(!menu.close(), "closing an closed menu is not a dismissal");
    }

    #[test]
    fn the_highlight_snaps_on_the_first_frame_and_glides_after_it() {
        let mut menu = open(&["one", "two"]);
        let first = [0.0, 0.0, 100.0, 40.0];
        assert_eq!(menu.animate_highlight(first, 1.0 / 60.0), first);

        let second = [0.0, 60.0, 100.0, 40.0];
        let stepped = menu.animate_highlight(second, 1.0 / 60.0);
        assert!(
            stepped[1] > first[1] && stepped[1] < second[1],
            "{stepped:?} should be on its way between the two rows"
        );
    }

    fn level(value: f32) -> Level {
        Level {
            value,
            muted: false,
        }
    }

    /// A track is a control on the panel, not a way off it: pressing one is
    /// still watched going down, but the panel it is on stays open — the answer
    /// to the press is the row itself, and a panel that folded away would take
    /// it with it.
    #[test]
    fn choosing_a_track_keeps_the_panel_open() {
        let mut menu = Menu::default();
        let entries = vec![
            Entry::new(Command::MuteApplication(7), "Something").level(level(0.5)),
            Entry::new(Command::Dismiss, "Close"),
        ];
        assert!(menu.open_at([0.0; 4], None, entries, 8));
        while menu.animate(0.05) < 1.0 {}

        assert_eq!(menu.choose(), Some(Command::MuteApplication(7)));
        assert!(menu.is_open(), "a mixer row does not hand the keys back");
        assert!(menu.press_progress(0).is_some(), "and is still watched");
        menu.backdate_press(PRESS_TIME);
        assert_eq!(menu.animate(1.0 / 60.0), 1.0, "and nothing folded away");

        // The row below it is an ordinary command and behaves like one.
        menu.move_selection(1);
        assert_eq!(menu.choose(), Some(Command::Dismiss));
        assert!(!menu.is_open());
    }

    /// The levels moving is not the panel changing: the highlight stays where
    /// it is, mid-glide if it is gliding, and a scrolled list stays where the
    /// user left it.
    #[test]
    fn a_track_moving_leaves_the_panel_alone() {
        let rows = |value: f32| {
            vec![
                Entry::new(Command::MuteApplication(1), "One").level(level(value)),
                Entry::new(Command::MuteApplication(2), "Two").level(level(0.2)),
                Entry::new(Command::MuteShell, "System").level(level(0.9)),
            ]
        };
        let mut menu = Menu::default();
        menu.open_at([0.0; 4], None, rows(0.5), 2);
        menu.move_selection(1);
        menu.move_selection(1);
        assert_eq!((menu.selected(), menu.first_visible()), (2, 1));

        assert!(menu.refresh(rows(0.6)), "the level moved");
        assert_eq!(
            (menu.selected(), menu.first_visible()),
            (2, 1),
            "nothing about the panel should have moved with it"
        );
        assert_eq!(menu.entries()[0].level, Some(level(0.6)));
        assert!(
            !menu.refresh(rows(0.6)),
            "and an unchanged listing is no news"
        );
    }

    /// An application that stops playing takes its row with it, and the
    /// highlight follows the row it was on rather than the position it was in.
    #[test]
    fn a_row_that_goes_away_takes_the_highlight_to_the_row_it_was_on() {
        let mut menu = Menu::default();
        menu.open_at(
            [0.0; 4],
            None,
            vec![
                Entry::new(Command::MuteApplication(1), "One").level(level(0.5)),
                Entry::new(Command::MuteApplication(2), "Two").level(level(0.5)),
                Entry::new(Command::MuteShell, "System").level(level(0.5)),
            ],
            8,
        );
        menu.move_selection(1);
        menu.move_selection(1);
        assert_eq!(
            menu.selected_entry().map(|row| row.command),
            Some(Command::MuteShell)
        );

        assert!(menu.refresh(vec![
            Entry::new(Command::MuteApplication(2), "Two").level(level(0.5)),
            Entry::new(Command::MuteShell, "System").level(level(0.5)),
        ]));
        assert_eq!(
            menu.selected_entry().map(|row| row.command),
            Some(Command::MuteShell),
            "the highlight followed the row rather than staying on row two"
        );

        // And a highlight whose row has gone, with nothing under it left to
        // take, falls back to the row before rather than off the end.
        assert!(menu.refresh(vec![
            Entry::new(Command::MuteApplication(2), "Two").level(level(0.5))
        ]));
        assert_eq!(
            menu.selected_entry().map(|row| row.command),
            Some(Command::MuteApplication(2))
        );
    }

    /// Pressing a row that removes itself is the ordinary way to work down a
    /// list — dismissing announcements one at a time is exactly that — so the
    /// highlight stays where it was standing and the row under it comes up to
    /// meet it. Sending it back to the top of the list would make the next
    /// press land somewhere nobody aimed, and the top of the notification
    /// panel is the row that throws the lot away.
    #[test]
    fn a_row_pressed_out_of_a_list_leaves_the_highlight_where_it_was() {
        let list = |ids: &[u32]| {
            let mut rows = vec![Entry::new(Command::DismissNotifications, "Clear All")];
            rows.extend(
                ids.iter()
                    .map(|id| Entry::new(Command::DismissNotification(*id), "Something")),
            );
            rows
        };

        let mut menu = Menu::default();
        menu.open_selecting([0.0; 4], None, list(&[4, 3, 2, 1]), 8, 1);
        menu.move_selection(1);
        assert_eq!(
            menu.selected_entry().map(|row| row.command),
            Some(Command::DismissNotification(3)),
            "opened under Clear All, and one row down from there"
        );

        // The row it was on is pressed and goes. The one that was under it is
        // now in its place, and that is what the highlight is on.
        assert!(menu.refresh(list(&[4, 2, 1])));
        assert_eq!(
            menu.selected_entry().map(|row| row.command),
            Some(Command::DismissNotification(2)),
            "and not back up on Clear All"
        );

        // All the way to the end of the list, and the highlight walks back up
        // rather than wrapping round to the top.
        assert!(menu.refresh(list(&[4, 1])));
        assert!(menu.refresh(list(&[4])));
        assert_eq!(
            menu.selected_entry().map(|row| row.command),
            Some(Command::DismissNotification(4))
        );
    }

    /// A row that leads onward: the panel keeps the keys, keeps its place, and
    /// does not change what it says until the row has been seen going down.
    #[test]
    fn a_further_list_arrives_only_once_the_row_has_finished_going_down() {
        let mut menu = open(&["open", "sort"]);
        while menu.animate(0.05) < 1.0 {}
        menu.move_selection(1);

        assert_eq!(menu.choose(), Some(Command::Placeholder("sort")));
        assert!(!menu.is_open(), "choosing always hands the keys back first");
        assert!(menu.descend(Some(Title::new("Sort")), rows(&["a-z", "z-a"])));
        assert!(menu.is_open(), "a step inward takes them straight back");
        assert!(menu.is_descending());

        // Half way down: still the list that was pressed.
        menu.backdate_press(PRESS_TIME * 0.5);
        assert_eq!(menu.animate(1.0 / 60.0), 1.0, "the panel must not fold");
        assert_eq!(menu.entries().len(), 2);
        assert_eq!(menu.title(), None);
        assert_eq!(menu.depth(), 0);

        // And once it has been: the further list, from the top, on a panel
        // that never went anywhere.
        menu.backdate_press(PRESS_TIME);
        assert_eq!(menu.animate(1.0 / 60.0), 1.0);
        assert_eq!(menu.title(), Some("Sort"));
        assert_eq!(
            menu.selected_entry().map(|row| row.command),
            Some(Command::Placeholder("a-z"))
        );
        assert_eq!(menu.depth(), 1);
        assert!(!menu.is_descending());
    }

    /// A list that had to be fetched is written over the wait, on the same
    /// panel, at the same depth.
    ///
    /// The step inward already happened when the row was pressed — what was
    /// stepped into was "asking Steam" — so this must not push a second list:
    /// Back from the tools has to reach the menu the press came from, and not
    /// a wait nobody wants to see again.
    #[test]
    fn a_list_that_had_to_be_fetched_replaces_the_wait_it_was_asked_from() {
        let mut menu = open(&["play", "compatibility"]);
        while menu.animate(0.05) < 1.0 {}
        menu.move_selection(1);
        menu.choose();
        assert!(menu.descend(
            Some(Title::new("A Game")),
            vec![
                Entry::new(Command::Placeholder("waiting"), "Asking Steam…").reading(),
                Entry::new(Command::Dismiss, "Cancel"),
            ]
        ));
        menu.backdate_press(PRESS_TIME);
        assert_eq!(menu.animate(1.0 / 60.0), 1.0);
        assert_eq!(menu.depth(), 1);
        assert_eq!(
            menu.selected_entry().map(|row| row.command),
            Some(Command::Dismiss),
            "the wait itself cannot be chosen, so the way out is"
        );

        assert!(menu.replace(
            Some(Title::new("A Game")),
            rows(&["Steam's choice", "Proton Experimental"])
        ));
        assert_eq!(menu.depth(), 1, "the same depth, not one further in");
        assert_eq!(menu.title(), Some("A Game"));
        assert_eq!(
            menu.selected_entry().map(|row| row.command),
            Some(Command::Placeholder("Steam's choice")),
            "and standing at the top of what arrived"
        );
        assert!(menu.back(), "Back reaches the menu the press came from");
        assert_eq!(menu.depth(), 0);
        assert_eq!(menu.entries().len(), 2);

        // And a list that arrives while the step inward is still in flight
        // replaces what is arriving, rather than the menu underneath it.
        let mut menu = open(&["play", "compatibility"]);
        while menu.animate(0.05) < 1.0 {}
        menu.move_selection(1);
        menu.choose();
        menu.descend(None, rows(&["waiting", "cancel"]));
        assert!(menu.replace(None, rows(&["Steam's choice"])));
        assert_eq!(menu.entries().len(), 2, "the menu it came from, untouched");
        menu.backdate_press(PRESS_TIME);
        assert_eq!(menu.animate(1.0 / 60.0), 1.0);
        assert_eq!(menu.depth(), 1);
        assert_eq!(
            menu.entries().len(),
            1,
            "and what steps in is what arrived, not the wait"
        );
    }

    /// Back is one level at a time, and the list is found as it was left —
    /// the same row highlighted, and a long one scrolled to the same place.
    #[test]
    fn back_returns_to_the_list_it_was_reached_from() {
        let mut menu = open(&["a", "b", "c", "d", "e"]);
        menu.set_window(2);
        for _ in 0..3 {
            menu.move_selection(1);
        }
        assert_eq!((menu.selected(), menu.first_visible()), (3, 2));

        menu.choose();
        menu.descend(None, rows(&["one", "two"]));
        menu.backdate_press(PRESS_TIME);
        menu.animate(1.0 / 60.0);
        assert_eq!(menu.entries().len(), 2);
        assert_eq!((menu.selected(), menu.first_visible()), (0, 0));

        assert!(menu.back());
        assert_eq!(menu.entries().len(), 5);
        assert_eq!(
            (menu.selected(), menu.first_visible()),
            (3, 2),
            "the list should be found exactly as it was left"
        );
        assert!(!menu.back(), "there is nothing behind the first list");
        assert!(
            menu.is_open(),
            "and Back off the end does not close it here"
        );
    }

    /// Changing your mind inside the press: the list that was asked for never
    /// arrives, and the panel is still the one that was pressed.
    #[test]
    fn a_step_inward_can_be_taken_back_before_it_lands() {
        let mut menu = open(&["open", "sort"]);
        menu.choose();
        menu.descend(None, rows(&["a-z"]));
        assert!(menu.back());
        menu.backdate_press(PRESS_TIME);
        menu.animate(1.0 / 60.0);
        assert_eq!(menu.entries().len(), 2);
        assert_eq!(menu.depth(), 0);
    }

    /// A further list with nothing choosable in it is refused, on the same
    /// ground an empty menu is: it would be a panel Back is the only way off.
    #[test]
    fn a_further_list_with_nothing_in_it_is_refused() {
        let mut menu = open(&["open", "sort"]);
        menu.choose();
        assert!(!menu.descend(None, Vec::new()));
        assert!(!menu.descend(
            None,
            rows(&["a"]).into_iter().map(Entry::disabled).collect()
        ));
        assert!(!menu.is_descending());
    }

    /// Putting the panel away abandons the whole journey, not the innermost
    /// list of it — a menu that folded away and came back one level deeper
    /// would be a menu that remembered something the user had left.
    #[test]
    fn closing_it_forgets_where_it_had_got_to() {
        let mut menu = open(&["open", "sort"]);
        menu.choose();
        menu.descend(None, rows(&["a-z", "z-a"]));
        menu.backdate_press(PRESS_TIME);
        menu.animate(1.0 / 60.0);
        assert_eq!(menu.depth(), 1);

        assert!(menu.close());
        assert_eq!(menu.depth(), 0);
        assert!(!menu.back());
    }

    #[test]
    fn a_reopened_menu_starts_from_the_top_again() {
        let mut menu = open(&["a", "b", "c"]);
        menu.set_window(2);
        menu.move_selection(1);
        menu.move_selection(1);
        assert_eq!((menu.selected(), menu.first_visible()), (2, 1));

        menu.open_at([0.0; 4], None, rows(&["a", "b", "c"]), 2);
        assert_eq!(
            (menu.selected(), menu.first_visible()),
            (0, 0),
            "the same button press should always leave it in the same place"
        );
    }
}
