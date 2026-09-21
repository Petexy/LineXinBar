# Screenshots, sharing and desktop integration

[Documentation](index.md) · [Project home](../README.md)

- [Screenshots](#screenshots)
- [Screen sharing](#screen-sharing)
- [Choosing a file](#choosing-a-file)
- [Showing a file](#showing-a-file)
- [Authorisation prompts](#authorisation-prompts)

## Screenshots

`Print` — the key with a picture of it on it — photographs the display the
keyboard is on, and it does so **whatever is held down with it**. Every desktop
spells a different variant of the picture with a modifier (the whole screen on
`Shift+Print` here, the clipboard on `Ctrl+Print` there, a region on
`Meta+Shift+Print`) and this shell takes one kind of picture, so a hand that
learnt any of those spellings is right. For a keyboard with no Print key at all
— a sixty-percent board — there is the chord every Mac has had for thirty
years, in both of the ways it gets transcribed onto a PC: `Ctrl+Shift+3`, with
Command read as Control, and `Alt+Shift+3`, with Command read as the key that
sits where it sits. Those two are ordinary chords and need their modifiers
exactly, because the key under them is a digit and belongs to whoever is
typing.

On a controller it is the **guide button with `R1`**, held together: the same chord a Steam Deck photographs a game with, and
the same two controls under every thumb — `STEAM`+`R1` on a Deck or a Steam
Controller, the PlayStation button and `R1` on a DualSense, Guide and `RB` on an
Xbox pad. It photographs the display that has control, which is the one the
shoulder buttons move between. The bumper on its own still moves to the next
display; a chord rather than a button of its own because there is no spare
control on a controller, every face button and both shoulders already belonging
to whatever is running.

The guide button is what pays for that chord: it is answered when it comes back
*up* rather than when it goes down, so that holding it can mean something. A tap
still opens the overlay and is still a tap; a hold spent on the chord opens
nothing at all. The alternative would be an overlay already drawn over the game
by the time the bumper arrived, and a photograph of the overlay.

It pays for two chords now — the other is [Steam's own
overlay](controls.md#the-guide-button-is-the-shells-alone), spelled with Select — and one
hold is spent by whichever of them lands. One hold and not one per pad: the
guide button is a single control however many controllers are plugged in, and a
chord is answered once rather than once for every device that could have spelled
it.

What lands in the file is what was on that screen: the wallpaper, the
application over it, and the shell's own bar or overlay over that, at as many
pixels as the display is being driven at. The pointer is not in it. The cursor
is drawn by the compositor rather than being part of anything, it is off screen
entirely while the session is driven from a controller, and an arrow burnt into
a picture cannot be taken back out of it.

The guide's menu over a window card offers the other picture: [Screenshot the
app](guide.md#the-context-menu), which is that application on its own — its own
contents at its own size, with nothing in front of it and no wallpaper behind.

Both land in the `Screenshots` folder inside the user's pictures — the folder
`xdg-user-dirs` recorded in the language the account was made in, so `Bilder`
on a German installation and `画像` on a Japanese one, never a second English
`Pictures` beside the real one. They turn up in the shell's own
[Images](shell.md#music-video-and-images) row like any other photograph on the disk.

Nothing pops up to say so. The display **flashes** instead, once, and the shell
sounds `screenshot.ogg` beside it — both after the file has actually been
written, which is why the flash is never in the picture it is answering for and
why the pair mean the screenshot happened rather than that a key was pressed.
Two senses because the chord is often spelled without looking: a person watching
the screen sees the flash, a person with their eyes on the game hears the
shutter. A panel would have to be drawn either behind the fullscreen application
it was reporting on, where nobody would see it, or in front of it with the
keyboard taken off whatever the user was doing.

Only the compositor can take either picture. A Wayland client reads its own
surfaces and nothing else, and the shell is a client: it cannot photograph even
the application it is drawn over. So both are requests over
[`lxb_shell_v1`](architecture.md#lxb_shell_v1), with the compositor supplying the pixels and
the shell the folder.

## Screen sharing

OBS records this session and Discord shares it, the same way they do on any
other desktop — which is to say through three pieces that have to all be there,
because none of them can do the job alone.

**The compositor speaks `wlr-screencopy-unstable-v1`.** A client asks for one
frame of one display, or of a rectangle of one, is told what buffer to bring,
and hands it over; the copy happens the next time that display draws. That is
also what paces a recording, so nothing has to guess the refresh rate. What
lands in the buffer is the composite, out of the same element list the display
draws, and whether the pointer is in it is the client's choice —
`copy_with_damage` really does wait, so a recorder pointed at a still screen is
given one frame and then nothing until something moves. Any tool that speaks
this protocol works: `grim`, `wf-recorder`, `wl-screenrec`.

**`lxb-portal` is the session's xdg-desktop-portal backend.** No application
speaks Wayland to share a screen: it asks `xdg-desktop-portal` over D-Bus, and
that hands the question to whichever backend the desktop installed. GNOME ships
one, KDE ships one, and this is LineXinBar's — `org.freedesktop.impl.portal.ScreenCast`,
answering with the number of a PipeWire node.

**The frames go over PipeWire, and are copied once.** Every buffer in the
stream is a memfd the portal allocates, and a memfd is exactly what a
`wl_shm_pool` is made from — so the memory PipeWire hands the application is
the same memory the compositor was told to copy the display into. The picture
goes from the GPU to the application in one step; nothing in the portal ever
touches a pixel.

The three files that make a session find it are in
[`share/`](../share/xdg-desktop-portal): the `.portal` file that names the D-Bus
backend, the `linexinbar-portals.conf` that says this backend answers screen
sharing and file choosing and leaves the rest to whatever else the machine has,
and a D-Bus service file so anything asking early can start it. The compositor
starts it beside the shell under `--shell`, because a portal is a client of
this compositor and has to be given the session's own display.

**Those files have to be installed, or nothing ever calls it.**
`xdg-desktop-portal` chooses its backends by reading `.portal` files out of the
data directories, and it does so once, at startup — a session whose portal was
never registered has `lxb-portal` sitting on the bus answering nothing, while
every application that asks is told there is no ScreenCast portal at all. That
is not an error anybody is shown: OBS simply offers no screen-capture source
and Discord's picker never appears. A package installs them; a session run
straight out of a checkout installs nothing, and
[`scripts/install-portal.sh`](../scripts/install-portal.sh) registers that build
for the current user instead (`--uninstall` takes it back). The portal says so
in the log when it cannot find its own registration.

**And the session has to tell the bus what it is.** A D-Bus activated service
inherits nothing from whatever asked for it — it is started by the bus, from
the bus's own snapshot of the environment — so a session that never updates
that snapshot has the portal come up with no `XDG_CURRENT_DESKTOP` and no
display, which lands in exactly the same place: a front desk with no screen
sharing on it. The compositor updates it (through
`dbus-update-activation-environment --systemd`) as soon as it owns the seat, or
when `LXB_PRIVATE_DBUS=1` says it has been given a bus of its own; nested on
somebody else's bus it leaves the snapshot alone, because rewriting it would
send *their* activated services into this compositor.

The other half of that is giving it back. The portal is one service per user
rather than one per session, and nothing restarts it, so a portal left running
with LineXinBar's answers is inherited by whatever the user logs into next —
and screen sharing is broken *there* instead, which is how this was found. A
session that owns the seat therefore stops `xdg-desktop-portal` on its way out,
and `lxb-portal` holds a connection to its own compositor for no other reason
than to exit when that connection breaks.

`lxb-portal --list-outputs` names the displays, and `lxb-portal --debug-cast
[DISPLAY]` shares one without a portal or D-Bus in front of it and prints the
node — which is how the pipeline is proved by hand with `gst-launch-1.0
pipewiresrc`.

**Nothing is shared until the user says so.** The portal cannot draw — the
shell owns the renderer, the glass and the panel every other question in this
session is asked through — so the question goes over `lxb_shell_v1`: the
compositor carries it to the shell, which opens the guide and puts up a panel
naming the application and offering one row per display, and carries the answer
back. The refusal is the row the panel opens on, so a press on a question
nobody read is a no. So is a question nobody answers, a session with no shell,
and a display unplugged between the question and the answer: there is no path
through `consent.rs` that shares a screen because something was missing.

One question at a time. The panel is modal and takes every button while it is
up, so a second application asking while the first question is on screen is
refused rather than queued behind a panel the user cannot see.

## Choosing a file

A browser wanting a photograph to upload, an editor wanting somewhere to save,
a game wanting a folder for its mods: none of them opens a file dialog of its
own on a Wayland session. It asks `xdg-desktop-portal`, which asks whichever
backend the desktop installed — usually without the application knowing,
because GTK's and Qt's own file dialogs quietly become portal calls when there
is a portal to call. This is LineXinBar's:
`org.freedesktop.impl.portal.FileChooser`, answering `OpenFile`, `SaveFile` and
`SaveFiles`.

**It is a session's own chooser because every other one needs a mouse.** GNOME's
and KDE's are windows with a tree, a list and a text field, and not one of those
is reachable from a controller. This session already has a file explorer that
is — the Files column — so the question is answered in *that*, framed.

**The panel is the explorer, at eight tenths of the display.** Cut from
[the context menu's own glass](guide.md#the-context-menu) through `sidebar_surface`, so
it is the same material as every other surface this shell raises. What is inside
it is [`crate::files`](../crates/lxb-desktop/src/files.rs) and nothing invented:
the same listing, the same folders-first order, the same search field at the
head of a column, the same New folder row where the folder can be written to.

**And it is drawn as the bar draws itself**, by the very same `pick_row` the
[folder picker](shell.md#carrying-a-file-somewhere-else) uses: a mark that grows under
the cursor with a bloom breathing behind it and a disc of glass beneath it, the
name beside it, and the line under the name only where the row is chosen. The
cross is the bar's cross, read against the panel instead of the display — every
column's chosen row sits on one line, so the trail reads straight across, and
the list slides under it rather than paging. It walks the same way too: Right
steps into a folder and opens a column beside the one it came from, Left steps
back out, and the trail of columns *is* the path. A panel of its own kind of
row would have been a second visual language for the one job this shell already
has a language for.

**A photograph is drawn as itself**, in the round hole its mark would have had
— the very shape a picture met in a folder wears on the bar, made by the same
worker for the rows around each column's cursor and no others. Which picture is
this is the question a chooser exists to answer, and a column of identical page
marks cannot. A row whose picture has not arrived yet, or never will, keeps the
mark for its kind; nothing about the row's size or place depends on the answer,
so the picture fades into a row that was already there.

**The foot says where you are, in full.** The whole path of the folder being
stood in — `/home/somebody/Downloads`, not `Downloads`, because a column headed
with a folder's name is one of several folders on the disk called that and what
the application is handed is a path. Under it goes what is showing, or what the
file will be called. Where a path is too long for the panel it is the one run in
the shell cut from the *front*: every path on a machine begins the same way, and
one cut at the end names the disk and never reaches the folder. Each column's
own heading sits over its **mark** rather than over its names — the mark is where
a column begins and what every row of it lines up on.

**The pane brings its own ground.** Every other pane in the shell is laid over
something the shell drew — the wallpaper it chose to be dark, or its own bar
over it. This one is laid over whatever the application that asked happens to be
showing, which on a video or a game is bright, moving and nobody's choice. So it
is cut to a panel's frost rather than the sidebar's (`FROST_PANEL` exists for
exactly this: *a panel has to carry text over anything at all*) and laid on a
sheet of near-black in its own shape. Neither is opaque: what is behind still
comes through, still moves, and is still recognisably the application waiting.
Over a test pattern filling the display, the two together took the variation
across the panel's own face from 31 luminance levels to 11.

Eight tenths rather than the whole display is the point of it: the application
that asked goes on drawing and goes on being seen round the edges, which is what
says it is still there and still waiting. The surface is lifted over that
application and holds the keyboard — every button belongs to the panel until the
question is answered, and the letters typed into its fields must not reach the
application underneath — but it is never called opaque, so the application is
not put to sleep behind it.

**Three things are the panel's own, and each because the question came from
outside.**

*A head row that answers.* For every purpose but "one file" the answer is not a
row of the listing — it is the folder being stood in, or the set that has been
ticked — so every column carries a row at the top that ends the question. The
same shape the [folder picker](shell.md#carrying-a-file-somewhere-else)'s Paste row has.
A column never *opens* on it, for that picker's reason: it is the one row that
acts, and a press of Accept out of habit would hand an application a folder
nobody chose.

Saving is the exception, and it is the only one. Walking into a folder in order
to write into it is a walk whose whole point is the folder just reached, so a
save opens the column on **Save here**, with the name already under it — and on
the name itself where there is nothing to call the file yet, because that is the
thing standing between the user and the answer. The rule holds everywhere else,
where the folder is only the way to the row being chosen and pressing it again
would answer with something nobody picked. A save cannot lose anything that way:
what it does is write a file under a name the user can read on the row above.

*The kinds.* An application may say it only accepts images; when it does, the
files that are not are left out, which kind is in force is written at the foot,
and the panel's own menu — the same button that raises one anywhere else, and
the right mouse button anywhere inside the panel — carries them under one row
called **Types**, which says what is showing and steps into the list. One row
rather than one row each: an application may offer five filters, and laid out
flat they pushed Sort and Show hidden files off the bottom of a short panel,
which is a menu that hides its own controls the more the application asks for.
**Everything** is always the last row of that list: a filter is the
application's guess at what the user wants and it is sometimes wrong, and a
chooser with no way past its own filter is one that hides the file the user is
looking at.

A kind is matched with `fnmatch`, brackets and all. That is not a nicety: a
filter is written for whichever matcher the toolkit that sent it uses, and
Firefox spells **every** filter it sends as a bracket per letter — an upload of
a photograph asks for `*.[pP][nN][gG]`, because that is how a case-insensitive
`*.png` is put to a matcher with no other way of asking. A matcher that read the
brackets as characters answered every one of those questions with an empty
column, which is what a folder full of photographs looked like here until the
filters were read off the session bus and the tests written from the capture.

*Nothing here destroys anything.* There is no Delete, no Rename, no Copy and no
Move. An application's file question is not a file manager, and a panel raised by
a web page should not be one press from emptying a folder. New folder is the one
exception, and only where the answer is somewhere to *write*: a save that cannot
make a folder is a save that can only ever go where something already is. The
trash is not offered either, for the reason no walk that is choosing something
offers it — an answer that disappears the next time the trash is emptied.

**The foot says what the buttons do, and names the ones in hand.** Four acts —
Select, Approve, Options, Cancel — each drawn as the control that performs it
rather than lettered, because the same act is South on a pad and the space bar
on a keyboard and no wording covers both: "press A" is wrong on a PlayStation
pad and meaningless to somebody typing. So the panel draws whichever the user
last reached for; see `settings::controller_in_hand`. Approve is absent where
there is nothing to approve — choosing one file is answered by pressing the file
— and a legend naming a button that does nothing would be worse than naming
none. **Options is on the legend because without it there was nothing on the
panel to say the menu existed**: an application asking for images showed a
folder of images and no sign that the sort order and the rest of the disk were
one press away. Its keyboard half is a mouse rather than a key, which is the one
place the legend leaves the keyboard — a menu is raised with the right button by
anybody holding a pointer, and no key printed on a keyboard says the same thing
to as many people.

Where a key is named it is named **as the key is labelled**: `Esc` is a word on
every keyboard ever made, so the mark is that word, while Enter and the space
bar carry the symbols printed on them. An earlier cut drew Escape as an arrow
leaving a keycap and it named nothing anybody could go and find.

**Where you are stands above the rule**, with the width of the whole panel to
itself. It shared the foot with the legend once, and a path sharing a line with
pictures of buttons is a path cut short to leave room for them.

**Accept and Approve are two different acts here**, which they are nowhere else
in the shell. Accept — South, or the space bar — presses the row under the
cursor: it steps into a folder, ticks a file, puts a name in the field. Approve
— Start, or Enter — ends the question with what has been chosen, from wherever
in the walk the cursor is standing. That is what makes a save two presses rather
than a walk back up the column to the row that answers, and it is why Enter and
the space bar part company on this one screen.

**Three ways out, and one of them is not announced.** Back cancels. A click past
the panel cancels — clicking past a thing to dismiss it is what every panel on
every desktop does, and one that ignored it read as one that had stopped
responding. And opening the guide cancels, silently: the guide outranks every
screen the shell draws and every grab an application can take, so a question
that could hold it off would be the one screen with no way out of it. Nothing is
said about that last one, because a panel explaining that the guide had
cancelled something would be the guide apologising for opening.

**Nothing is handed over until the user says so.** The road is
[`ask_to_share`](architecture.md#lxb_shell_v1)'s exactly: the portal cannot draw, so the
question goes over `lxb_shell_v1`, the compositor carries it to the shell over
the top of whatever is filling the screen, and carries the answer back as paths
the portal turns into `file://` URIs. Back cancels, and so does a question
nobody answers, a session with no shell, a shell that goes away mid-question,
and a second application asking while a panel is already up. All of them reach
the application as the same thing: it was given no file.

It will not interrupt a journey the user is already in, either. A file being
carried to a folder and a panel waiting on an answer are both the shell holding
a question of its own, so an application that asks over one of those is told it
was given nothing and may ask again. The guide and a context menu are not
journeys and are simply taken away.

`lxb-portal --debug-pick [one-file|many-files|folder|new-file]` puts a question
to the shell without D-Bus or an application in front of it and prints what came
back — the counterpart of `--debug-cast`, and how this half is proved by hand.
`--kind 'Images=*.png'` offers a kind, `--at` opens it somewhere, `--called`
names a new file.

**And the registration has to name it**, which is its own trap. The `.portal`
file is installed separately from the binary and `xdg-desktop-portal` reads it
once at startup, so a backend that has grown an interface since the last install
answers a front desk that has never heard of it — and the application is quietly
handed another backend instead, which is exactly what a session with no portal
of its own looks like. That is not hypothetical: this shipped against a
registration naming ScreenCast alone, and every Save dialog in the session
opened GTK's. `lxb-portal` now compares what it answers against what its own
registration claims and says so in the log when they disagree.

## Showing a file

The other direction, and the one the desktop had backwards. A browser finishing
a download offers **Show in folder**; an archiver that has just unpacked
something offers **Open containing folder**. Neither opens a file manager. Each
calls one method on one well-known bus name:

```
org.freedesktop.FileManager1.ShowItems(["file:///home/…/thing.zip"], "")
```

**On a machine with a normal desktop installed, that name is activatable.**
Dolphin, Nautilus and Thunar each ship a D-Bus service file claiming it, so a
session that answers nothing does not get silence — it gets Dolphin, started on
demand, drawn over the shell, in a session with no window management for it and
no way back. That is what pressing Show in folder used to do here.

**So the shell holds the name.** It takes `org.freedesktop.FileManager1` at
startup, on the session bus, beside the notification daemon and the polkit
agent — and for the same reason all three are the shell rather than a process
next to it: Files is not a program to start, it is a column of the bar, four
rows into System, drawn by the shell out of the shell's own tree. Nothing else
in the session can put it on a screen or knows which display the user is
driving. A name already owned is never activated, so nothing else is started
behind it. The name is taken without replacing an existing owner, so a
LineXinBar run inside somebody else's desktop for testing leaves that desktop's
file manager alone.

**And then the shell walks there.** The cursor is carried into Files, the disk
holding the path is chosen — the deepest one, so a download opens under Home
rather than four columns down from Root — and one column is read and stepped
into per part of the path. Only when it has arrived does the bar come forward,
over whatever application was in front. That order is the point: what the user
sees arrive is the folder they asked for, rather than the bar arriving on
whatever it was last left on and then being seen to rummage through the disk.
The columns still slide open, because that is what the bar does when a column is
stepped into; nothing was invented for this.

The file itself is left under the cursor, with its size and the date it was
written on the row — which is also this desktop's answer to `ShowItemProperties`,
the one method here that does something other than what its name says. There is
no properties window in this shell, and what one would be opened to read is
already on the row. `ShowFolders` stands *in* the folder instead of pointing at
something in it.

**Three methods, one place to stand.** Several URIs in one call come down to the
first: the shell has one cursor per display and one row can be under it. A URI
that is not `file://`, a path that is not absolute and a name that is not on
this disk are each refused on the bus, on the call that made them, rather than
becoming a bar pointed at nothing. The escaping is undone as *bytes* — a file
name on Linux is bytes, and decoding into a string first would refuse every file
this shell can list whose name is not UTF-8.

It will not interrupt a journey the user is already in, on exactly the terms the
[file chooser](#choosing-a-file) will not: a file being carried, a panel waiting
on an answer, or an application's own file question each own the columns this
walk would move, and each was started by the person at the machine — where this
was started by a program behind whatever is on screen. Nor over a launch splash
or a window still flying home, both of which own the display until they hand it
over themselves.

One thing is read differently from anywhere else in the shell. **Show hidden**
is off by default, and a walk to something under `~/.local/share` with it off
reaches a column where the very folder it needed next was left out — it would
stop three levels short with nothing under the cursor, which is the shell
appearing to have lost a file it was handed the whole path of. So a path with a
dot-name anywhere in it is read with the dotfiles in, for that walk and no
other. Nothing is written down and the setting is not touched.

**The other road is `xdg-open` on a folder**, and that one is not a bus call. It
looks up whatever the machine says opens `inode/directory` and starts it, which
is also what `xdg-desktop-portal` falls back to when no file manager answers.
Two files cover it, both in [`share/applications/`](../share/applications):
`linexinbar-files.desktop`, which claims the type and whose `Exec` is
`lxb-desktop --show-in-files %u` — not a shell starting, but one call to the
shell this session already has, and out — and `linexinbar-mimeapps.list`, which
names it as the default.

That list is deliberately *desktop-specific*. A file called
`<desktop>-mimeapps.list` is read only while `XDG_CURRENT_DESKTOP` lowercases to
`<desktop>`, and LineXinBar's session sets `XDG_CURRENT_DESKTOP=LineXinBar`. So
installing this package takes folders away from nothing else on the machine, a
KDE session on the same disk keeps Dolphin, and the user's own `mimeapps.list`
is never written to. A package installs both files; a session run straight out
of a checkout installs nothing, and
[`scripts/install-file-manager.sh`](../scripts/install-file-manager.sh) registers
that build for the current user instead (`--uninstall` takes it back). The bus
name half needs none of that — the running shell takes it either way.

## Authorisation prompts

Mounting somebody else's disk, installing a package, restarting a service: none
of those are done by the program the user pressed. It asks `polkitd`, which
reads the action's policy, and where the policy says a human has to agree,
`polkitd` asks *the session's authentication agent* for proof. A desktop with no
agent registered is a desktop where every one of those is refused with nothing
on screen to allow it, which is what this was until the shell grew one.

**The agent is the shell itself.** It is the one part of LineXinBar with a D-Bus
connection of its own — the portal owns the rest of the session's D-Bus, and
deliberately — and the reason is what travels back: a *password*. The shell holds
one in a single allocation that never moves, never prints it, and overwrites it
when it is dropped; carrying it across a Wayland connection, through the
compositor and into a second process would undo all three. So `polkitd` is
answered on a thread of its own, and what is typed goes from the field straight
down the helper's socket without leaving the process.

**The password is checked by polkit, not by the shell.** Every agent hands it to
`polkit-agent-helper-1`, which is the only part of this that runs as root and the
only part that talks to PAM — over `/run/polkit/agent-helper.socket` where the
machine's polkit is socket-activated, or by running the setuid helper where it is
not. The helper tells `polkitd` directly whether the password was right, so
nothing the shell can get wrong turns a "no" into a "yes": the most a bug here
can do is fail to ask.

**The panel is the one a removal uses**, because it is the same question: a
padlock, what the authorisation is for in polkit's own words, whose password is
wanted, and a field drawn as one mark per character. The on-screen keyboard comes
up with it — on a console there is nothing else to type a password with — and the
guide is opened underneath, so the question is readable over a game filling the
screen. It opens on Cancel: a panel that appears without being asked for must not
have the answer that hands over authority sitting under somebody's thumb.

A password PAM refuses is offered again, exactly as a removal offers a refused
`sudo` password again, and PAM's own complaint is what the panel says when it has
one. A helper that fails *before* asking for anything is a machine that could not
be asked at all, and says so rather than looping. Cancel, Back and Escape all
decline, and a question `polkitd` withdraws — because whoever asked gave up —
takes the panel away with it. Every one of those routes ends the helper, which
leaves PAM without the answer it was waiting for.

Two things are worth knowing. **A second question waits** rather than being
refused: the shell asks one thing at a time, and the next is put up as soon as
the panel is free. And **the agent registers for the login session** where there
is one, falling back to the shell's own process — which is what a machine with no
`logind` gets, and what a LineXinBar started inside another desktop gets, since
`polkitd` allows one agent per session and that desktop's got there first.

