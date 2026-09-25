# Applications, media and files

[Documentation](index.md) · [Project home](../README.md)

- [The shell](#the-shell)

## The shell

`lxb-desktop` binds `zwlr_layer_shell_v1`, so it is not tied to LineXinBar —
it runs on any compositor implementing layer-shell, which also makes it
debuggable on its own.

The bar opens with Settings, LineXinBar's own column, which holds the shell's
settings the way a console's Settings region holds its own. Everything after it
comes from `.desktop` files in the usual XDG search path, grouped into the
categories Plasma's launcher uses: System, Multimedia, Graphics, Internet,
Office, Games, Software, Development, Education & Science, Utilities, Waydroid
and Other. A category with no application in it is hidden — except Settings,
which is part of the bar rather than a result of what is installed.

Two of those columns are not filled by a main category, because no main
category answers what they are about. **Software** holds the stores and
software hubs — anything declaring `PackageManager` alongside `System` or
`Settings`, plus a short list of the well-known stores that declare neither,
Plasma's Discover among them. **Waydroid** holds the Android applications this
machine can run: Waydroid writes `X-WayDroid-App` on every entry it generates
and no main category at all, so before that column they fell through to Other.
Waydroid's own launcher is filed with them, at the head of what it runs. An
entry Waydroid marked `NoDisplay` stays hidden, as it does everywhere else in
the shell.

Which column a session opens on is **Settings > System > Startup category**,
and it is Games unless somebody has said otherwise. The page is the bar itself —
one row per column this machine has, each wearing its own mark — and a column
named by the setting but not on the bar this session, a Steam library nobody is
signed in to, is offered all the same and says why it is not there. Where the
named column is genuinely absent the shell opens where it always did: the first
column with something in it.

An application can also ask for its icon to be drawn in the shell's own
material rather than as the picture its theme holds, by naming `lxb` in its
`Keywords` or `Categories`. The icon is then measured into a distance field and
the quad shader cuts a bead of water to it, exactly as it does for the shell's
own marks, so the row wears the same material as the column heading above it.
It is opt-in because what the shader is handed is the *silhouette*: a drawing
made to be a shape comes out as one, and a photograph comes out as a rounded
slab of glass.

Multimedia carries two subcategories of its own, Music and Video, Graphics
carries one, Images, and System carries **Files**. What is in all four is the
user's own files rather than applications — the first three by kind, gathered
from everywhere under `$HOME`, and the fourth by where they actually are. No `.desktop` file can say which half of Multimedia an
application belongs to — the menu spec requires `AudioVideo` alongside `Audio`
or `Video` but never the reverse, so an entry may declare `AudioVideo` and
stop, and many of the best-known media applications do exactly that — so the
players and the editors stay in their columns, where nothing has to be guessed
about them.

### What the buttons do

The start screen writes its own controls in the corner opposite the clock:

```
                                  Select  (A)   Options  (Y)   Guide  (⌂)
```

**Drawn rather than lettered.** "Press A" is wrong on a PlayStation pad, which
has no A, and worse than wrong on a Nintendo one, where A is the button on the
opposite side of the cluster from where an Xbox layout puts it. So each is a
picture of the cluster with the button in question filled in, which is true on
all of them — the same drawings the file panel's own legend uses, laid out by
the same code, because it is the same promise: somebody who has learned that the
filled bead at the bottom of the cluster takes a row must not have to learn it
again in a file dialog.

The stick is the one exception, and it is the exception that proves the rule.
Both sticks sit on the same face of the pad, so a picture of one is a picture
of the other and position says nothing — it is drawn in profile, with the `R`
that every pad prints on that side of itself and a triangle over it, because
the right stick *pushed* and the right stick *pressed* are two different acts
here. A side is not a button name: it does not move between layouts and no
maker brands it.

**And named by whichever control is in hand.** The same act is South on a pad
and Enter on a keyboard, and there is no wording that covers both without naming
neither, so the shell says the one the user's hands are actually on — it watches
for a button, a stick or a key and remembers which came last. On a keyboard the
row reads Enter, the right mouse button and Super. Options leaves the keyboard
deliberately: no key printed on a keyboard says "menu" to as many people as the
right button does. Super is the one key with a mark for it that is not somebody's
logo, and it is the binding the compositor holds back from every application so
that the guide is always reachable.

**Options comes and goes with the row.** Most of the bar is objects — an
application, a song, a file, a game — and every one of them has a context menu;
a settings value and a subcategory are not objects and have none. The legend
offers the button exactly when a press on it would raise something, because it
asks the very function the press does. A legend naming a button that does
nothing is worse than naming none.

**Guide never does.** It is the one press that works from everywhere in the
session, an application holding the whole screen included, and a legend that
dropped it on some rows would be hiding the way out.

It is drawn on the display being driven and no other, and it gives the corner up
to anything with buttons of its own: the guide overlay, a context menu, the
centred panel, a file being carried, an application's file question — which
draws a legend of its own — and the on-screen keyboard's corner chip, which
stands in this very corner. Two clusters of button pictures in one corner is not
a legend, it is a pile.

**It can be turned off** at `Settings > System > Button hints`, and it is on
until it is. Somebody who does not need it is exactly the person who will find
the switch; somebody who does will never go looking for a setting to reveal what
they do not know is missing. One answer for the whole session: the same switch
takes the row off the guide, the friends panel, the file question, the keyboard
chip and the launch splash, and it reaches applications built on the toolkit
too, through `shell.toml`.

The guide draws its own row, in the mirror of this corner and by these rules —
see [what the menu's buttons do](guide.md#what-the-menus-buttons-do), where the
one screen that has three rows rather than one is set out.

### Music, Video and Images

All three rows list what is on the disk: every audio, video or image file
anywhere under `$HOME`, alphabetically, with the folder it came from written
under its name. A file's kind is its extension, and its row opens it in
whatever the user has already set as their handler for that type — the default
from `mimeapps.list`, then any installed application that declares the type,
preferring one that calls itself a `Player` or a `Viewer` over one that calls
itself an editor, then `xdg-open`. So a song opens in mpv rather than Audacity
and a photograph in Gwenview rather than GIMP, without the user having chosen
either.

One walk fills all three: the expensive half is reading the directories, and
what is in them decides which row a file lands on. It runs on a worker thread
from the moment the shell starts and what it finds is hung on the bar as it
arrives, so stepping into Music never waits on the disk. It repeats every five
minutes, which is how a file copied in during a session turns up and how one
deleted goes away again. Hidden folders are skipped, as every desktop's
indexer skips them, and symbolic links are not followed.

Five minutes is the right interval for a collection nobody is looking at, and
the wrong one in the two cases where somebody is. So neither of those waits:

- **A file this session wrote is shelved as it is written.** The shell asked
  for the screenshot and knows the path in the answer, so that one file goes
  straight onto the shelf — a picture taken while standing in Images appears in
  the column a moment later, under its own thumbnail, without anything being
  searched for.
- **Stepping into a shelf brings the next walk forward.** Opening Music, Video
  or Images is somebody asking what they have, and the answer should include
  the film they recorded a minute ago in an application this shell knows
  nothing about. It is the same walk started early rather than a scan of its
  own, and it cannot happen more than once every twenty seconds, so a column
  stepped in and out of ten times still costs one pass. Walking *past* the row
  asks nothing: idle scrolling must not read the disk.

A row holds everything found, however much that is. Nothing about a frame the
shell draws depends on how much music somebody owns: a column is a screen tall,
so the drawing and the hand both walk the rows that can be on the display and
never the list, and a picture is made only for the handful either side of the
cursor.

The shelves live on the worker, not in the shell. It walks, it keeps what it
finds, it puts the chosen order on, and it builds the rows themselves — and
what crosses back is a finished list, which the shell swaps into the bar in a
few microseconds however long it is. Everything that costs anything in
proportion to the size of a collection happens on that side of the wire: the
merge, the sort, the search, letting go of the list that was replaced. The
shell sends four things the other way — list this shelf differently, show only
what matches this, this file has been deleted, and here are the rows you can
let go of now — and holds no files at all.

This is what the first two seconds of a session are made of. On the home
directory it was written against, half a million pictures, the shell used to
spend about 1.4 of those two seconds blocked: a quarter of a second to rebuild
the rows, four times a second, on the thread that draws. It now spends four
microseconds per delivery and drops no frames. Deliveries also thin out as the
shelves grow, since a shelf that takes a tenth of a second to build is not
worth rebuilding four times a second for one more file nobody could pick out of
half a million — the walk hands over eight times instead of fifty-seven.

Everywhere under `$HOME` means everywhere: a repository checked out in the home
directory has its screenshots and its SVGs listed like anything else, since
nothing about where a file sits says it is not a picture. Icon and dump formats
(`.ico`, `.xpm`, `.pbm`) are left out, and so are `.ts` and `.mts` — both name
MPEG transport streams and both name TypeScript sources, and a development
machine would otherwise file several thousand of the second kind under Video.

#### Searching a shelf

Each of the three columns carries a field at its head, and pressing `A` on it
raises the on-screen keyboard, because on a console there is nothing else to
type with. What is typed goes onto the row itself rather than into a panel over
it: the row *is* the field, and it carries the caret while the board is up.

A column of music still opens on music. The field stands over the list where
anything standing over a list stands, and is reached by pressing Up from the
top of it — the one direction nothing else was using, and where a person looks
for the thing above the first thing. Opening on it instead would make every
visit to a shelf begin by stepping over a control nobody asked for. The same
goes for a shelf that has just been reordered: what "newest first" asks to be
shown is the newest file, not the search.

Walking off the field ends the typing, and the board goes with it. That is
asked once a frame rather than of any particular way of leaving, because the
bar can be walked out from under an open keyboard by routes that never touch
the field — a pointer resting on the category row is one, and control passing
to another display is another.

The column narrows as each letter lands. A file matches when what was typed
appears anywhere in its name, ignoring case, so `radio` finds both *Radiohead*
and *Old Radio Show*. The folder is not searched — an album answering to the
name of the shelf it sits in would bury the one track that was meant — and
neither is anything on another shelf: a search of Music returns audio files
because the shelf it narrows holds nothing else. The line under the field says
how much of the collection is being kept back (`2590 of 517488 images match`),
and the row that the column hangs on says it too, so a narrowed column can be
told from a small one without stepping into it.

`Enter` puts the board away and takes the cursor to the first match. Back does
the same without moving it. Neither undoes the search: the column has been
narrowing in plain sight with every letter, so there is no earlier list still
on screen for a cancel to mean — what puts the whole shelf back is the **Clear
search** row, which is on the column for exactly as long as there is something
to clear. A search lasts the session and is not written to the settings file:
an order is a preference, and a search is something somebody is doing.

The narrowing itself happens where the files are, on the same worker and for
the same reason as the sort. Half a million photographs are filtered per
keystroke there rather than between two frames, and the field does not wait for
it — the letter is on the bar on the frame the key was pressed, and the rows
under it are whatever the worker last finished. So the two are never
inconsistent with each other, only with the future.

#### Thumbnails

Video and Images draw the files themselves rather than a mark standing for
them: a row is a card with a frame of the film or the photograph on it, fitted
to its own shape so nothing is cropped and a portrait picture stays portrait.
Music does not, because getting cover art out of an audio file means parsing a
tag format per container — and a track is picked by its name anyway.

Nothing is made ahead of time. Each display asks for the rows within four of
its cursor, once a frame; two workers make those and nothing else, and the
atlas holds only what is on screen. Scrolling a thousand photographs therefore
costs the same as looking at six, and a library of any size costs nothing at
all until somebody opens the column.

What is made is written to `$XDG_CACHE_HOME/thumbnails/large`, in the layout
the freedesktop thumbnail specification lays down — a PNG named for the MD5 of
the file's URI, carrying the source's URI and modification time so a stale one
can be told from a good one. That is the same cache every file manager fills,
so on a machine where the user has browsed their pictures in Dolphin they are
already there, and the ones this shell makes are still there afterwards.

Photographs are decoded in-process (PNG, JPEG, GIF, WebP, BMP, TIFF, and SVG
through the same renderer the shell's own glyphs use). Films need
`ffmpegthumbnailer` or `ffmpeg`; without either, and for a format nothing here
decodes — AVIF, HEIC, camera raw — the row keeps the column's glyph.

A machine with music on it but no media player installed gets its Multimedia
column back the moment the first file is found, and the same goes for Graphics
and a photograph: a column is hidden for having nothing in it, not for having
no applications in it.

#### What the menu over a file offers

`Y` on one of these rows raises a menu of six, in two bands — four that act on
the file and two that do not:

**Open** starts it in whatever a plain `A` would have used. **Open with** lists
every installed application that says it handles the type, best first, each
under its own name and its own icon; the one at the top is the one Open would
take, and wears the same tick the Settings column puts on a value in force. The
row is greyed where nothing installed claims the type, since there is then
nothing to choose between.

Choosing one of them opens nothing. It records that application as the default
for the type — written into the user's own `mimeapps.list`, where every other
desktop keeps it, so it holds for the file manager and the browser too and it
holds after a restart. Open is what acts on it, which is the whole difference
between the two rows: one plays the file, the other answers "which program
plays these", and a list that did both would make changing the answer cost a
window every time.

The list stays up, and the tick moves to the row that was pressed. It is a
setting being made rather than a command being run — the same bargain the
mixer's tracks strike, where the row changing *is* the answer — so it is the
user's to leave, by Cancel or `B`, when they are satisfied. The rows keep the
order the panel opened in while it is up, even though the newly chosen one now
belongs at the head: a list that reordered itself under a press would move the
row somebody was already reaching for.

**Delete** asks first, and what it does is move the file to
`~/.local/share/Trash` in the layout the freedesktop trash specification lays
down — the same trash Dolphin, Nautilus and `gio trash` fill, so a file put
there by this shell can be restored from any of them by somebody who has never
heard of LineXinBar. A file on a volume mounted inside `$HOME` goes to a trash
at the top of *that* volume, because a rename cannot cross a filesystem and
copying forty gigabytes to delete it is not a deletion. Nothing is ever
unlinked and nothing is ever copied; if the move fails the file stays exactly
where it was and the panel says so. The row is greyed for a file outside the
user's home directory. The row it was on goes at once rather than at the walk's
next pass, so the bar is never still offering to play something the user has
just watched themselves delete.

**Rename** opens a field on the row and is described [with the folder
listings](#changing-a-name), where it does the same thing to the same effect.

Copy and Move are **not on this menu**, and are missing rather than greyed —
the one place in the shell where a row that exists elsewhere is simply not
drawn. A shelf is a library rather than a folder: it gathers one kind of file
from everywhere the user keeps them, so there is no column for the picker to
open in and none for the file to appear in when it lands. Greyed is the shell
saying "not at this moment", and it is owed the user an idea of what would make
it the right moment — install something that opens this type, or look at a file
that is your own. There is nothing of the kind here: the answer would be "go
and find this song in Files", which is a different column about a different
thing, and two dead rows on every song, film and photograph in the machine is a
high price for a panel that keeps one shape.

Below the rule, **Sort** and **Cancel**. Sort is about the column rather than
the file, which is what the rule is saying.

#### What order the rows are in

Alphabetical to begin with, and nine orders altogether: name either way, size
either way, type, created either way and modified either way. Chosen from the
Sort row and remembered per shelf in `[media-sort]` of the shell's settings
file, because how somebody wants their music listed says nothing about how they
want their photographs listed.

Size and both dates are read once, when the walk first sees a file — one
`stat` per media file per session, and none at all on the passes after. Type is
the mime type rather than the extension, so `.jpg` and `.jpeg` are one group.
A file the disk knows no date for sorts last in *both* directions of a date
order, because "no date" is not an early date; and an order the whole shelf has
nothing to answer with is drawn greyed rather than offered and silently doing
nothing — several filesystems keep no creation time at all.

The order is put on when the rows are drawn, not when the shelf is filled. The
shelf itself is always alphabetical, because that is the order it is merged in:
what the walk finds arrives in whatever order the filesystem answered, and
merging a batch into a list already in the same order is one pass over both
rather than a re-sort of everything.

Choosing an order shows the column from its first row. Somebody who has just
asked for the newest first is asking to be shown the newest, and a cursor held
on whichever file it happened to be standing on would answer with that file's
new position instead. Only the display the order was chosen on moves; any other
screen showing the same shelf keeps its file, because the order changed
underneath it rather than at its request.

### Subcategories

A column is a tree, not a list. A row can be a subcategory, and choosing one
opens its own column beside it: the bar slides over, the column behind keeps
only the row it was opened from, and what is left on screen is where the user
came from, reading left to right under the category it hangs off.

```
Settings
   |
Appearance  >  Accent color
```

Stepping further in slides the whole chain along by one, so the column just
opened stands where the cross always is and the one it came out of takes the
place its own parent had. A path four subcategories deep therefore costs no
more of the screen than a path one deep — the far end of it goes off the side
rather than the near end marching across.

What is behind does not merely dim, it *recedes*: one step further back per
column opened in front of it, drawn smaller and hazier each time, with the
category row a step behind the outermost column of all. One subcategory in,
the row is two steps back and its column one, so how far the path has gone is
answerable at a glance and not only by reading it.

That is what the Settings column is made of, so it stays a few rows deep
instead of becoming one long list. Right steps further in and Left steps back
out — but only once there is a path to walk. At the top of a column the two
still belong to the category row, because a column of nothing but
subcategories would otherwise be one you could never get past.

A list of values opens on the value in force and marks it, so what the shell is
set to is where the cursor already is. Highlighting another previews it; the
palette flows into that value while the mark and saved setting stay where they
were. Pressing accept moves the mark, applies the value and remembers it in
`~/.config/lxb/shell.toml`. Stepping back or left without accepting flows
back to the applied value instead.

### Files

System's first row opens the disk itself. Where Music, Video and Images answer
"what have I got", this answers the other question — "what is *in there*" — and
it is the same tree of columns that answers it: a folder is a subcategory, a
file is a row, and the trail of columns behind the cursor is the path.

```
System  >  Files  >  Root  >  usr  >  lib
```

It opens on three kinds of place. **Home** is the user's own folder; **Root**
is `/`; and after them comes a row per drive, mounted or not. A mounted one
says how much room is left on it. The list is built on the press that opens it rather
than when the shell starts, so a stick plugged in half an hour into a session
is on it the moment somebody goes looking.

A drive is a mounted filesystem backed by a device node, or a network share.
`/` is not one of them, because it is the Root row; nor is any of the mount
points a machine makes for itself (`/boot`, `/home`, `/usr`, `/var` and the
rest), which on a machine with subvolumes would otherwise offer the same disk
five times under the names of its own directories. Loop devices count only
under `/run/media`, `/media` and `/mnt`, where they are an `.iso` somebody
attached rather than a packaged application the system mounted.

#### Drives nothing has mounted

A drive nothing has mounted is on the list too, in the place its name puts it
among the others, reading **Not mounted · 932 GiB**. Pressing it mounts it and
steps into it. The row reads **Mounting…** while that happens, and a second
press is spent rather than asking twice. A disk inside the machine makes the
system ask for an administrator's password first, on the shell's own panel; a
USB stick does not. If the drive cannot be mounted, a panel names it and says
so in plain words — the reason UDisks gave is in the journal. The cursor is only
taken into the drive if it is still on its row when the mount finishes.

```
System  >  Files  >  GamesHDD (Not mounted · 932 GiB)  --A-->  GamesHDD  >  …
```

A USB stick or a card is mounted the moment it is plugged in, and one already
plugged in when the session starts is mounted then, without asking for anything
— the way GNOME and KDE behave. One the person unmounted stays unmounted until
they open it.

The menu over a drive has the one way to put it away: **Unmount** for a disk
inside the machine, **Safely remove** for one that can be unplugged, which
unmounts everything on it and turns it off, then says it can be pulled out.
Something still using the drive is said in words, with what to do about it.
Whether the machine mounts a drive by itself when it starts is under
[Settings > Storage](settings.md#storage).

The list comes from UDisks, the service GNOME's and KDE's file managers mount
through; see `crates/lxb-desktop/src/drives.rs`. Left out are what UDisks says
to ignore (firmware partitions, Windows' recovery), anything that is not a
filesystem, a drive whose mount-table line says `x-gvfs-hide`, and a disk image
attached by another account. An encrypted partition is not offered yet: it needs
its passphrase before it can be mounted, and that is a separate change.

Inside a folder the subcategories come first, then the files. A folder says
when it was last written until it has been opened, and afterwards what was
found in it (`239 folders, 5804 files`); a file says how big it is and when it
was written. Files of a kind the shelves already know keep that shelf's mark —
a song is drawn as a song here too — an archive is drawn as a carton with its
lid on, and everything else is a page. A mark is for a kind the shell can do
something with: there is a shelf for a song and an
[Extract](#getting-what-is-inside-an-archive) for a `.tar.gz`, and what a `.pdf`
is, is written on the row in words. Dotfiles
are left out: nobody's photographs are in `~/.cache`, and the shell's own
settings are a column of their own.

Every column here carries the same field at its head that the three shelves do,
and it means the same thing: `A` on it raises the on-screen keyboard, what is
typed goes onto the row itself, and the column narrows as each letter lands. A
name matches on any part of it, ignoring case. The row above goes on saying
what is in the folder while the field says what is being shown of it — `4 of 6
items match` — and the **Clear search** row under it is there for exactly as
long as there is something to clear.

The field is never what a column opens on. It stands over the list where
anything standing over a list stands, and stepping into Files puts the cursor
on Home, one row below it — the same arrangement a column of music has, and for
the same reason: opening on the field would make every visit begin by stepping
over a control nobody asked for. Pressing Up from the first row is how it is
reached.

A search belongs to the looking somebody is doing rather than to the folder.
Stepping out and back in is a fresh visit and shows everything that is there,
because a column that arrived already narrowed by what was typed in the last
one would be hiding files with no field in sight to say so.

Narrowing a folder is *cheaper* than opening one. A shelf is half a million
files held on a worker, so a search there is a message and the rows arrive when
they are ready; a folder is one directory, so the search is one more `readdir`
— and only the rows the query keeps cost the `stat` behind a size and a date.

**Sort** is on the menu over any file, and it offers the same nine orders the
shelves do: name either way, size either way, type, created either way and
modified either way. It is about the column rather than the file, as it is on a
shelf — and about every folder rather than this one, because an order is how
somebody reads a list and a shell that had to be told again in each directory
would be asking them to say the same thing over and over. It is remembered in
`[media-sort]` of the settings file under `Files`, beside the three shelves'
own orders.

Folders stay above files in all nine. Two of them a directory cannot answer
for — it has no type, and what `stat` gives for its size is the size of the
index rather than of what is in it — so in those the folders keep the
alphabetical order somebody can find them in, and only the files are ranked.

A folder is read on the press that opens it, on the thread that draws, and
never on the way past — a cursor walking down `/usr` passes a hundred
directories on its way to one. An ordinary folder takes under a millisecond
and the largest on a stock machine, `/usr/lib` with six thousand entries,
takes sixteen: one frame, once, on a press somebody made. Nothing is cached, so
what is on the bar is what was on the disk when it was pressed, and the folders
beside the one being opened give up their rows as it opens — the tree holds the
path the user is standing in and not everywhere they have been. A listing stops
at ten thousand rows and the row above says how many were left out.

`Y` over a file offers the shelves' menu with two more rows in it — Open, Open
with, Delete, Copy, Move, Rename, Sort, Cancel — and every row it shares means
here what it means there. **Copy and Move are the two, and this column is the
only place they appear at all**: a row here stands in a folder rather than in a
library, so there is somewhere for the picker to open and somewhere for the file
to appear when it lands. Delete is greyed
outside the user's home directory, which in this column is most of what can be reached — the
machine's own files can be looked at from here and never destroyed from here.
Copy and Move are not greyed with it: taking a copy of one of the machine's own
files is not destroying it, and whether the folder chosen will have it is the
filesystem's answer to give at the moment the transfer runs rather than the
menu's to guess.

`Y` over a **folder** offers a shorter list — Copy, Move, Rename, Sort, Cancel — and
only inside a listing: Home, Root and the drives are rows of the same kind
carrying the same kind of path, and none of them is a folder anybody may pick
up. There is no Open, because a folder is not opened by a program and pressing
it already opens the column; and no Delete, because a folder is however many
files deep and "do you want to delete *this*?" cannot honestly be asked about a
name standing for a thousand things nobody can see. Where both rows lead is
[the folder picker](#carrying-a-file-somewhere-else).
Opening a file starts it in whatever the desktop already opens that type with,
exactly as pressing it on a shelf does, and the loading screen carries the
file's own name.

A photograph or a film in a folder is drawn as itself, in the round hole its
mark would have had. Not as a card: a shelf of photographs is a column of cards
because everything on one *is* a picture, and a folder holds folders, documents
and photographs together — a column that was half cards and half rows would be
answering two shapes at once. So the rows stay rows, and the picture takes the
place of the mark of its kind.

The hole is square and a photograph almost never is, so the middle of it is
shown. Fitting the whole picture inside the circle would leave two empty
crescents round it, which down a column reads as pictures of several different
sizes; stretching it to fill would give the wrong face. The middle is what
every gallery on every phone shows in a grid, and it is where the subject of a
photograph nearly always is.

They are made the same way the shelves' are and out of the same cache — the
rows within four of each display's cursor, two workers, nothing ahead of time —
so a folder of six thousand files costs six thumbnails.

#### Carrying a file somewhere else

Copy and Move are the first two rows of that menu that need a *second* place
named before anything can happen, and where they lead is the same bar again,
mirrored.

The screen it opens is one row and one column. The thing being carried stays
where it was picked, alone, with the rest of the start screen taken away from
around it — everything but the corner's clock, which is part of the wallpaper
more than it is part of the controls. Beside it, in from the right, comes a
column of the folder it is in now, with **Paste** standing at the head of it
where a shelf's search field stands. What is under the head row is the folder
itself: the folders in it, which can be walked into, and the files, drawn
quieter — they are there so the folder is recognisable, and there is nothing
behind a press of one, because nothing can be filed inside a file.

**A column never opens on Paste.** It opens on the row below it, whatever that
row is, and the file being carried is one press of Up away from being filed.
Paste is the one row on this screen that acts: a column that opened on it would
put the end of the journey under the user's thumb at every step of the journey —
walk into a folder to see what is in it, press `A` out of habit, and the file
has been filed somewhere nobody chose. Reaching for something is what says it
was meant. A folder with nothing in it has only the one row, and there the
selection has nowhere else to be.

```
                     osu.appimage          Paste  ·  Copy here      AppImages
                                           osu.appimage
                                           r2modman.appimage
```

It is driven the way the bar is driven, with one thing the other way round:
the columns of the path stand to the **right** of the one being stood in, so
**Right is the way back out** and Left is the way in. Two lists of folders both
walking left would be the same gesture meaning two different things on one
screen; mirrored, the hand knows which of the two it is driving without being
told. Right goes back as far as `/` and no further — the picker is a path, and
every path on this machine ends there. There is no Volumes row at the top of
it: a mounted drive is under `/` and is reached by walking down to it.

Pressing Paste ends the journey. `B`, `Escape` and the right mouse button give
up on it, wherever on the display the click lands — the whole screen belongs to
the transfer while it is up, so there is nowhere on it a right button could be
asking about something else.

Paste says under itself what pressing it would do — *Copy here*, *Move here* —
and where it cannot be pressed it says that instead, greyed: **It is already
here** for a move into the folder the file is already in, and **It cannot be
put inside itself** for a folder being carried into itself or into anything
under it, which would copy until the disk was full.

A name already taken in the chosen folder stops the transfer and puts a
question up: **Keep both**, **Replace**, **Cancel**, in that order and opening
on the first, because it is the only one of the three that cannot lose
anything. Keep both writes `osu (2).appimage` — the number before the
extension, where every desktop this sits beside puts it. Replace is drawn warm
and in the shell's fixed red, and it is a promise about the *name*: a folder
written over a folder is merged into it rather than put in its place, because
deleting the hundred files already in there is not something the panel
mentioned and not something a press can be taken to have asked for. A copy into
the folder the file is already in is not a clash at all — it is a duplicate,
which is a thing people ask for, and it lands beside itself under a free name
with nothing asked.

The copy runs on a thread, because a file is as big as it is. Nothing is shown
for one the disk finishes inside a couple of frames; past a third of a second a
panel says what is being carried, and it stays until the transfer ends. Ending
says nothing and shows nothing — the user watched themselves choose a folder,
and a panel telling them it worked would be a button to press to get back to
the screen they were already on. What they get instead is the column, read
again, with the file in it or gone from it. A failure *is* a panel, carrying
what the filesystem actually said, because a command that silently either
worked or did not is a command nobody trusts twice.

#### Changing a name

**Rename** is on the same menu, and on the shelves' as well — the one of these
three rows that is: a song has a name wherever it is being looked at from,
whereas Copy and Move need a folder to have been opened. It opens a field on the row itself rather
than a panel over it, which is the same thing pressing a search field does —
what a press means there is "I am about to type", and the answer to that is a
keyboard.

What it opens with is what the row was showing. In a folder that is the file
name and all of it; on a shelf it is the title without the extension, because
that is what the row says — nobody thinks of a song as `Yesterday.flac`, and a
field that opened with an extension the user had never been shown would be
asking them to look after something the shell had been hiding. The `.flac` goes
back on the end of whatever they write.

`B`, `Escape` and the right mouse button put the old name back and give up. This
is the one thing a search field does *not* do, and the difference is what the
two are: a search has been narrowing the column with every letter, so there is
no earlier list left to return to and Escape does not pretend otherwise; a name
has changed nothing at all until it is accepted, so giving up costs nothing and
is therefore free. Nothing reaches the disk until Return.

Three names are refused before the filesystem is asked — nothing, `.` and `..`,
and anything with a `/` in it, which is the one byte a name cannot hold. Return
on one of those leaves the caret where it is and says nothing, because what is
wrong with the name is on the row in the user's own letters. A name something
else in the folder already has is a panel and the end of the rename: `rename(2)`
would replace that file without a word, and unlike a copy there is no "keep
both" to offer — the user asked for *this* name.

Afterwards the row goes and comes back rather than being edited in place, for
the reason a deleted file's row goes at once: what the bar holds is what was on
the disk when it was read. A folder is read again on the spot, one `readdir`,
and the cursor is put back on the row wherever the new name has moved it to —
the column is alphabetical, and a name is exactly what that order is on. A shelf
is half a million files on a worker, so the worker is told the one thing it
needs — this path is gone, that one has arrived — and the rows arrive on the
frame it has them.

A move is a rename where a rename will do, whatever the file is the size of.
The fallback is the one case it cannot be — the two paths on different
filesystems, which on any machine with a stick plugged into it is most of what
a move is *for* — and there it is a copy followed by taking the original away,
in that order, so a failure leaves the user with two copies rather than none.
Symbolic links are carried as links rather than as what they point at: a folder
of shortcuts copied the other way could be a hundred times the size of what
somebody thought they were carrying.

#### Getting what is inside an archive

Pressing a `.zip`, a `.tar.gz`, a `.rar` or a `.7z` does not start a program.
Nobody wants to *look* at an archive — they want what is inside it — and the
only part of that the shell cannot work out for itself is where the contents
should go. So the press is answered by a panel with the archive's name on it and
two ways out: **Extract here**, which means the folder the archive is sitting
in, and **Choose a folder**, which is the same mirrored bar Copy and Move are
carried on, with **Extract** at the head of every column where Paste would
otherwise be.

Those rows are recognisable before they are pressed. An archive wears a carton
with its lid on where an ordinary file wears a page — the fifth object in the
explorer's set, and there on the same rule the other four are: a mark is for a
kind the shell can do something with, and what wears it is exactly what Extract
opens, read off that one list so a row can never promise a box the press cannot
open. It is the carton the panel's own mark is emptying, shut.

It is drawn seen a little from above, which is how the drum a drive is drawn one
row up is drawn and for the same reason: the lid of a box is a face rather than
a line, and a box drawn flat-on with a lid seam across it is a mouse — two
buttons and a wheel — before it is anything else. That was the first drawing and
it had to go.

Nothing is written over, ever, and that is why the panel asks "where" rather
than "are you sure". The archive is emptied into a hidden staging directory
inside the chosen folder, and only what comes out of it is moved into place: one
thing in the box takes a free name beside its neighbours, so a `holiday.tar.gz`
holding a single `holiday/` arrives as `holiday/` and not as `holiday/holiday/`,
and a `report.pdf.gz` arrives as a PDF rather than as a folder with a PDF in it;
anything else keeps the box, under the archive's own name, so a zip of two
hundred loose files cannot empty itself over somebody's Downloads. A name
already taken takes the next free one — `holiday (2)` — exactly as a copy does.
The archive itself is left exactly where it was.

When it is done the bar is standing inside what came out. A copy or a move ends
with the column read again, because what changed is a row in a column the cursor
was already in; an unpacking makes a folder, and the folder was the point of the
press — somebody who pressed `holiday.tar.gz` wanted the photographs, not a row
saying `holiday` to step into by hand. So the shell steps into it for them, by
the walk *Show in folder* arrives by: the trail eases one column further in with
the contents under the highlight, and from **Choose a folder** it is carried
across to wherever that folder was. An archive that was one file in a coat has no
folder to stand in, so the cursor is left standing on the file it became.

The shell does not know how to unpack anything. It knows how to ask the thing
that does, which is the same arrangement removing an application uses: one
family of archive becomes one argv, run on a thread, reporting one outcome.
`bsdtar` — libarchive with a command line on it — is asked first wherever it
will do, because one program that is already under every package manager reads
tar, zip, 7z, rar, iso and cab alike; where it is missing the ladder falls to
`tar`, `unzip`, `7z`, `unar` or `unrar`, and a bare `.gz`, `.xz` or `.zst` goes
to the program that made it. Where the machine has *nothing* that can open the
format, the press is answered by a panel naming the package that would fix it
rather than by silence.

**Extract is an application as far as the rest of the machine is concerned.**
It is what the Open with list offers first for an archive, wearing a tick like
any other answer in force, and somebody who would rather press a `.zip` and get
Ark can say so there and have it stick — the choice is written into their own
`mimeapps.list` like any other, and from then on Open starts Ark and no panel
appears. Extract stays on that list underneath, so the choice can be made and
unmade; this desktop ships an entry named `linexinbar-extract.desktop` for no
other reason than that a `mimeapps.list` names desktop entries and an answer
with no name could be displaced and never chosen back. That entry is also the
road in from outside the shell: `xdg-open` on a `.zip` in a LineXinBar session
runs `lxb-desktop --extract`, which unpacks it where it stands and asks nothing,
because there is no screen there to ask on.

