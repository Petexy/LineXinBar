# Guide overlay and context menus

[Documentation](index.md) · [Project home](../README.md)

- [The guide overlay](#the-guide-overlay)
- [The context menu](#the-context-menu)
  - [The resolution something draws at](#the-resolution-something-draws-at)

## The guide overlay

A launched application fills the screen and owns the keyboard, so there has to
be a way back out of it that the application cannot swallow. Pressing the
controller's guide button, or the compositor's `guide` binding, brings up a
menu over whatever is running:

| Entry             | Effect |
| ----------------- | ------ |
| Stick pointer     | Whether the right stick is a mouse inside the application in front |
| Volume mixer      | Opens [a panel](#the-volume-mixer) of everything making a noise, a row per application |
| Do not disturb    | Whether anything may interrupt. On, an announcement is filed without a bubble and without a chime |
| Notifications     | Opens the list of what has been announced to the session, newest first. Wears a mark while anything on it has not been looked at |
| Volume            | How loud the session is — a bar, moved with Left/Right; `A` mutes. The [volume keys](#quick-settings) move this same control from anywhere |
| Brightness        | How bright *this* display is, where that can be changed |
| Resume            | Dismiss the overlay |
| Close *app*       | End the application on the selected card. It is asked first and cannot refuse |
| Start screen      | Show the start screen over the running application, without closing it |
| Power             | Suspend, turn off, or end the session |

Close and Start screen are offered only while the card beside the column is a
window. The start screen is the last card in the deck, and it is neither
something to close nor something for Start screen to bring up that Resume does not
already. Nothing but the power button ends the session: `Esc` opens this menu
rather than quitting, so leaving is always a deliberate choice.

Close names the application on the *selected* card rather than the one in
front, and ends it rather than asking it to go: it is sent the polite close a
window manager sends, and `SIGTERM` then `SIGKILL` four seconds later if that
is ignored. What is ended is the application rather than the window's own
process — killing the window Steam draws leaves `steam` to put up another one —
so the compositor works out what the application is before it works out how to
end it. The grace is there so an application can finish writing, not so it can
decline.

The first four are square tiles sharing one line at the head of the column
rather than rows of their own, centred on the panel: Up and Down treat the line
as one stop on the way down the column, Left and Right walk it, and a tile that
can do nothing from where the user is standing is stepped over rather than
stopped on. Only the stick pointer is ever in that state, because it is the one
tile about the application in front — the mixer always has the session's own
output on it, the notification list answers *nothing arrived* as readily as it
lists what did, and whether the session may be interrupted is a question with an
answer on an empty machine.

The bell carries the one mark in the column: a bead in its corner while
something has been announced that nobody has looked at, the badge every phone
puts there. No number on it — what is worth knowing from across a room is
*something arrived*, and how many there are is a question the list itself
answers, one row per line, a press away. Opening the list is what counts as
looking at it; a bubble in the corner of the screen deliberately does not,
because it appears whether or not anybody is in the room. The mark is white
rather than accent-coloured so that walking the selection onto the very tile it
is pointing at does not make it disappear into the light.

Two of the four are switches, and a switch says which state it is in by being
*filled* rather than by being lit, since the selection is already lit. Do not
disturb is the one that outlives the application it was thrown over: it is
written to `shell.toml` and comes back with the next session, because a console
that had quietly turned it off overnight would deliver a night of announcements
at breakfast. What it stops is the bubble in the corner and the sound that goes
with it — one answer, given in one place, so neither can be silenced without the
other. Nothing is discarded: everything still arrives, and the tile beside it is
where it is read.

The header is two columns. On the left the wall clock, and under it whatever is
running on this display; on the right the day, and under *that* the battery — so
the left of it is what the session is doing and the right is what is true about
the machine underneath. On a machine with no battery the right-hand column is
the day and nothing else.

It is the same mark and the same reading the start screen's corner draws — see
[Battery percentage](settings.md#battery-percentage), which is the one setting behind both.
What differs is where the figures go when they are turned on: **left of the
mark** here, and above it in the corner. The corner is a cluster on a wallpaper
with a whole display beside it, where stacking the number keeps it from pushing
the clock inward; this is a narrow column with a line of its own to spend and
nothing above the mark but the date. A long application name gives up exactly
the room the mark takes and ends in an ellipsis rather than running under it.

### What the menu's buttons do

The overlay writes its own controls in the bottom-right of the display, in the
corner and on the line [the start screen writes its own in](shell.md#what-the-buttons-do)
— the same inset, the same ink, mirrored off the bottom. It is the same
promise made one press further in: somebody who has learned that the filled bead
at the foot of the cluster takes a row must not have to learn it again inside
the menu. The same `Settings > System > Button hints` turns both off.

```
                        Select  (A)   Options  (Y)   Friends  (X)   Back  (B)
```

The way out is **Back**, not Guide: on the bar that pair names the way *in* to
this menu, and here the reader is already in it. Options comes and goes with the
card, on the bar's own rule — it is there while the light is on a window's card,
which has [a menu](#the-context-menu) about that window, and gone on the
trailing start-screen card, which is not a window. Friends is there where there
is a Steam account for [the panel](steam.md#who-is-on-steam-and-talking-to-them) to be about. A
context menu or the centred panel takes the row away; the power dialog does not,
because it is part of the menu and every word the row is carrying is still true.

**And it is a different row while the buttons are pointed somewhere else.** A
[video floating over the menu](settings.md#picture-in-picture) is a second thing
the same buttons can act on, so the corner says so:

| What the buttons are on | The row |
| ----------------------- | ------- |
| The menu, with a video floating over this screen | Select · Options · **Picture-in-Picture** · Friends · Back |
| One of the videos | Options · Friends · Back |
| A video following the thumb, because its menu asked it to | Done · Cancel |

The middle pair of the first row is the press that goes to the videos — the
right stick pressed, or `P` — and it is drawn where the friends list is drawn
and for its reason: it is somewhere else to stand rather than something done to
what the light is on, and it is not the way out. It is there only where there is
something floating on this screen to go to.

On a video, **Select is gone**: the press is swallowed, because there is no card
under the light to take and one that fell through would launch whatever the menu
happened to be standing on. Options stays, and means the same thing said about
the window instead — it raises that window's own menu, the one the right button
raises. Back is still the way out of wherever the reader is standing, which is
now the video and back to the menu behind it.

While a window is following the thumb, everything else is held back, so the row
is the two presses that end it: one leaves the window where it has been put and
the other puts it back where it was picked up from.

The row does **not** step back with the menu while the directions are on a
video. Everything else in the overlay dims, which is how the shell says the
thumb is somewhere else — but the row is then the one part of the screen that is
*about* the live half, and dimming it would have quieted the only thing still
answering a press.

### What is coming down, and what the machine is doing to itself

The far corner from the column, and the one thing here that is not part of the
menu: while a Steam game is downloading, a card stands in the bottom right
wearing the game's own icon, with what is arriving, a bar, and the percentage.
It is a reading and not a control — no selection stops on it and no press
reaches it — and it is drawn only while this menu is open. It is described with
the rest of the download in [Steam](steam.md#steam), where the numbers on it come from.

An update installing in the background gets the same card, wearing the update
mark and saying which source is being updated — see
[Updates](updates.md#using-the-page). The two are two things happening and
neither takes the other's place, so they stack: the download keeps the corner it
was drawn for, the update stands on top of it, and when the download has
finished leaving the card above comes down into the corner rather than being
found there on the next frame. The row of button pictures in that corner climbs
over whichever of them reaches highest.

### The cards

Beside the entry column stands a second one: a card per window on this display,
one above another, with the start screen as the last card of all. Right leaves
the entries for them and Left comes back; Up and Down step the cards, which stop
at their ends rather than wrapping, and the selected card is always centred with
its neighbours peeking in past the top and bottom edges — the cut-off card is
what says there is more to scroll to.

`A` on a card goes back to that window, so the guide is also the window
switcher; `A` on the last card is the bar, over the running application or
plainly if there is none. Close, and [the menu](#the-context-menu) the top face
button raises, are both about the *selected* card rather than about whatever is
in front.

`Alt+Tab` is that switcher reached the way every desktop reaches it. The chord
brings the overlay up with the deck already in front and the highlight on the
card *behind* the one on screen; each further `Tab` while Alt is held steps one
card on, `Alt+Shift+Tab` steps back, and letting Alt go takes whatever the walk
landed on — that application coming to the front with the keyboard, or the start
screen where the walk ended on the last card. The walk wraps, alone among the
ways of moving in the deck: a held modifier is a question rather than a held
direction, and a fourth `Tab` on three applications is asking to come back round
to the first, not to be told there is nowhere further to go. The compositor
watches for the modifier coming up, because by then the overlay it is being let
go of over is the shell's and the application is no longer being sent the keys.

They are the live windows rather than pictures of them. The compositor animates
each window into its slot and draws it there, out of the same layout crate the
shell decorates the slots from, so the two processes paint one composition
without either sending the other any pixels; leaving flies every window back to
where it was. Which is also why opening the guide wakes everything on the
display up — see [What draws, and when](architecture.md#what-draws-and-when).

### Quick settings

The two bars go through the interfaces that exist below a desktop, because in
this session there is no desktop to ask — no settings daemon, no applet, no
`org.kde.*` or `org.gnome.*`.

- **Volume** is whichever of WirePlumber (`wpctl`), PulseAudio (`pactl`) or the
  kernel mixer (`amixer`) answers first, in that order. The last needs no sound
  server running at all.
- **Brightness** is the kernel's backlight class for a screen wired into the
  machine, and DDC/CI over the monitor's own i2c bus (`ddcutil`) for one that is
  plugged into it. The bar belongs to the display the menu is on, matched by DRM
  connector, so the monitor whose brightness moves is the one being looked at.

Either bar is left out of the column entirely when nothing on the machine can
move it, rather than drawn dead. A backlight is only offered if it can actually
be written to, which usually means being in the `video` group; DDC/CI usually
means the `i2c` group and the `i2c-dev` module.

All of it runs on a worker thread. A monitor can take half a second to answer
on i2c, and the bar moves the moment a key is pressed rather than waiting for
it.

Set `LXB_BACKLIGHT` to a directory under `/sys/class/backlight` to name
the backlight device outright, for the machines where two devices describe one
panel or where the screen cannot be recognised as built in.

The volume also has the keys it is printed on: `XF86AudioRaiseVolume`,
`XF86AudioLowerVolume` and `XF86AudioMute`, whatever is held down with them.
They work from wherever the user is — the compositor holds those bindings and
forwards them over `lxb_shell_v1`, because a fullscreen application owns the
keyboard and is usually the thing being turned down. Held down, the two
directions repeat at the session's own key repeat rate; mute is a switch and is
thrown once.

A press moves the same control the guide's row moves, by the same twentieth,
and raises it at the foot of the display for a second so there is something to
read while it moves. That control is the sidebar's own bar on a pane of its
own; while the guide is open it does not appear at all, because the row in the
sidebar is already showing it. Nothing else of the shell comes up with it — the
start screen stays behind the application, and the keys neither give the shell
focus nor swallow a click.

The overlay is drawn by the shell, not the compositor, so it uses the same
renderer and fonts as the bar. It moves its layer surface to the overlay layer
with `Exclusive` interactivity and draws onto a transparent surface, which is
what lets the running application stay visible through the scrim.

There is one overlay, on the display holding control — the other displays carry
on showing their own bar. `L1` / `R1` and `Tab` / `Shift+Tab` work from inside
the menu, and it follows control to the next display; with more than one
display it names the one it is on.

Everything it says is about that display: the cards are the windows *there*, it
offers to resume or close one of those, and it reads *Nothing is running* on a
display that has none, whatever is on the others.

Two input paths reach it, because neither alone is enough:

- **Controllers** are read straight from `/dev/input` rather than through
  Wayland, so the guide button arrives even while a game holds the keyboard —
  as do the three chords spelled on it and on Select: the on-screen keyboard's,
  [the screenshot's](desktop-integration.md#screenshots), which is wanted in that state more than in
  any other, and [Steam's own overlay](controls.md#the-guide-button-is-the-shells-alone),
  which means nothing in any other state at all. Every other control is ignored there, so the bar cannot react
  behind a running game. That the game cannot read the guide button *at all* is
  a separate piece of work on the pad itself — see
  [the guide button is the shell's alone](controls.md#the-guide-button-is-the-shells-alone).
- **Keyboards** go to the focused application, so the shell would never see the
  key. The compositor therefore owns the `guide` binding and forwards it over
  `lxb_shell_v1` (see below).

### The volume mixer

The tile beside the stick pointer's opens a panel of everything the machine is
playing: one row per application, each under the name and the icon the bar
already knows it by, with the shell's own audio in a band at the foot. Left
and Right slide a row, `A` silences it and brings it back, and the panel stays
up while they do — a track answers by *changing*, and a panel that folded away
on the press would take the answer with it.

A row is an application rather than a sound. A browser with three tabs playing
has three streams open in the sound server and is one thing anybody wants to
turn down, so they are grouped by the program behind them and the row moves all
of them together — which also means a tab falling silent does not renumber the
panel under the user's hand. What the row is called is what the desktop entry
that installed it is called, not what the stream calls itself: the bar has the
name the user chose it by, and the server has "Zen" or "Music Player Daemon".

It is [the same panel](#the-context-menu) every context menu is drawn as,
because it is the same kind of object — a short list about one control, grown
out of that control. What makes it a mixer is the rows.

The row at the foot, `System`, is [the shell's own audio](architecture.md#shell-audio) — how
loudly the interface answers and the start screen's background music plays. It
is not the machine's output, and that is deliberate: what the whole session
comes out at
is the volume bar a few rows above it in the same sidebar, which is there
whether or not this panel is opened, and a row that turned the machine down as
well would be the same control twice while leaving the shell's own audio with
none. It is also the one row here that no sound server knows about, which is why
it is in a band of its own and why it is written to `shell.toml` rather than
left with the mixer. Sliding it is its own preview: the click a direction makes
is heard at the level that direction has just moved it to.

That row is always there, which is why the tile is never dimmed — the
applications come and go with what the machine is playing, and the shell is the
one thing on the list that is certainly there, being the thing drawing it. The
application rows come from `pactl list sink-inputs`, which PipeWire answers as
well as PulseAudio; a machine with neither — the `amixer` case in
[Quick settings](#quick-settings) — gets a mixer with the shell's own row on it
and nothing else, because per-application volume is not something the kernel
mixer has.

The shell is in that listing too, as `ALSA plug-in [lxb-desktop]`, and it is
struck out of the applications: it is already on the panel as `System`, and two
rows over one thing would be one control too many — the one a user reached for
first being the one that does not last, since the server forgets a stream the
moment it closes and the shell opens a new one for the next click. It is
recognised by the process the server names it under, and, on a server that
names no process at all, by the program in the brackets the ALSA plug-in puts
it in.

### The stick pointer

A console has a controller and no mouse, and plenty of what runs on one was
written for a mouse — a browser, an emulator's menu, a settings page. The tile
at the head of the guide's column turns the controller into one, inside the
application in front:

| Control                    | What it does |
| -------------------------- | ------------ |
| Right stick                | Moves the pointer |
| `R2` — the right trigger   | Left click |
| `L2` — the left trigger    | Right click |
| Left stick, or the D-pad   | Scrolls what is under it |

The triggers because a handheld's desktop mode clicks with them, so somebody who
has held one reaches for them without being told — and because they are the two
controls a thumb on the aiming stick is not using. Nothing else is taken: the
shoulders still move between displays, the stick presses are the floating
video's, and the left-hand face button is still half of the keyboard's chord.

They used to be `A` and `B`, and the change is not about familiarity. `A` and
`B` are the on-screen keyboard's — one presses the key under its cursor and the
other puts the board away — so a pointer that borrowed them had to hand them
back for as long as the board was up, which is exactly when a mouse is wanted:
the one thing to do with a pointer while a keyboard is on screen is to click the
field about to be typed into. The triggers mean nothing to the shell anywhere,
so nothing has to be shared and nothing is ever handed over.

The scrolling is continuous rather than a count of wheel notches, because a
stick is: pushed further to scroll faster, exactly as a touchpad is dragged
further. The D-pad counts as a stick pushed all the way over, so holding a
direction runs down a page rather than nudging it.

It is a choice made per application, not a mode the shell is in, because the
right stick is not free: in a game it is the camera. So it is remembered
against the application rather than the session, in
`~/.config/lxb/apps.toml`, keyed by the name the application gives itself:

```toml
[apps."org.mozilla.firefox"]
stick-pointer = true
```

The key is the `app_id` an `xdg_toplevel` sets, or an X11 window's class, which
the compositor reports over `lxb_shell_v1`. Not the window title, which is
a document name and changes under the setting. An application that sets neither
cannot be remembered at all — one entry shared by every nameless window on the
machine would be worse than none.

Which is also what the tile does with nothing in front of it to be about: it is
drawn, dimmed, and the highlight steps over it rather than stopping on a switch
that cannot be thrown. [The mixer](#the-volume-mixer) beside it never is,
whatever is running, because the panel it opens always has the session's own
output on it. Both are still drawn, because a line that came and went with the
application would slide the other tile across the sidebar every time something
was closed.

The pointer is the seat's own, not a drawing of the shell's: `wl_pointer`
belongs to the compositor, and the point is to reach the application. The shell
therefore reads the stick from `/dev/input`, as it reads the guide button, and
hands the movement over as `move_pointer` — after which it is an ordinary
pointer movement, constraints and relative-motion stream included, so a client
cannot tell it from a mouse.

It cannot be walked out of the application either. A mouse belongs to the
session and may be pushed onto any screen there is; this stick was turned on for
one window, and there is nothing outside that window it was ever aimed at —
least of all the other display, where the cursor arrives somewhere the user is
not looking. So the movement stops at the edge of the window in front of the
display being driven, which is the same window the setting itself is filed
under.

The mouse is still a mouse, though, and may leave the cursor anywhere. So the
first push of the stick after that fetches it back, to the nearest point of the
window rather than to the middle of it: the pointer is where the hand left it,
brought just far enough to be somewhere the stick can reach. A click does not
fetch it, because a click has to answer to the cursor the user can see.

The stick goes back to being a stick the moment one of the shell's own screens
is in front of the application — the guide menu, or a launch still waiting for
its window. There is nothing on either to point at, and the pointer stays
exactly where it was parked.

The on-screen keyboard is the one thing drawn over an application that does
*not* take the pointer away, because it does not need to: it is driven with the
D-pad, the left stick and `A`, and it has no use for the right stick or for
either trigger. So only the wheel and the arrows wait for it. Aiming and
clicking carry on underneath the board — which is how a field is chosen before
it is typed into — and the scrolling comes back when `B` puts the board away.

One honest limitation, and it is the same one that makes the guide button work
at all: a controller is not routed through Wayland, so the application is
reading `/dev/input` too. Turning this on in something that uses the right
stick itself moves the pointer *and* the camera. It is meant for the
applications that ignore controllers entirely, which are exactly the ones that
need a mouse.

## The context menu

`A` does the one thing a row is for. Everything *else* that can be done to it
lives behind the top face button — `Y` on an Xbox pad, `Triangle` on a
PlayStation one — which is where a console shell of this shape has kept its
options menu since the first one. On a keyboard it is `Y` or `F10`, deliberately *not* the
`Menu` key: that one summons the guide, and with Steam running it is the only
thing that does so without Big Picture coming up alongside it.

It is a component rather than a screen. Whatever raises it hands over three
things: the rectangle of the control being acted on, a name for it, and the
rows. The panel then grows out of that rectangle, stands beside it on whichever
side of the display has room, and folds back into it when the menu is answered
or dismissed. Adding a command later is one line in a list; raising a menu
somewhere new is one function that returns those three things.

Six of them exist so far:

| Where | What it is about | Rows |
| ----- | ---------------- | ---- |
| The bar | The application on the focused tile, out of the disc it stands on | Information, Uninstall, [Resolution](#the-resolution-something-draws-at) / Launch, Close |
| The bar | One of the user's own files, on a shelf | Open, Open with, Delete, [Rename](shell.md#changing-a-name) / Sort, Cancel |
| The bar | One of the user's own files, inside a folder listing | Open, Open with, Delete, [Copy, Move](shell.md#carrying-a-file-somewhere-else), [Rename](shell.md#changing-a-name) / Sort, Cancel |
| The bar | A folder inside a listing, out of the same disc | Copy, Move, Rename / Sort, Cancel |
| The guide | The window under the selected card, out of that card | Move to next display, Move to previous display, [Open as Picture-in-Picture](settings.md#picture-in-picture), Screenshot the app / Cancel |
| The guide | [Everything making a noise](#the-volume-mixer), out of the mixer tile | One row per application / the session's own output |
| Anywhere | A [floating window](settings.md#picture-in-picture), out of the window itself | Move, Resize, Realign, Full screen, Move to next display, Move to previous display, Close / Cancel |

The last is a menu in shape and material and not in kind: its rows are tracks
rather than commands, so it is slid rather than pressed and it is raised by `A`
on the tile rather than by the button below. Nothing about the panel had to
change to carry it, which is the point of it being a component.

A row can also lead to a *further* list rather than doing something — Open with
and Sort both do. The panel stays exactly where it is and swaps what is written
on it, once the row that asked has been seen going down; `B` steps back out to
the list it came from, and only closes the panel from the outermost one.

The rows themselves are ordinary furniture. A row can carry a glyph, sit in a
band of its own below a hairline, be warm for something there is no coming back
from, or be offered and out of reach — "Move to next display" is drawn as an
outline on the last display rather than being left out, so the command is still
discoverable and the menu keeps its shape on every screen. The highlight steps
straight over anything it cannot stop on, wraps at both ends, and glides between
rows rather than jumping. A list
longer than the display can hold scrolls under a panel that does not change
size, with an arrow at whichever end still has rows past it.

### The resolution something draws at

Three of those menus carry a **Resolution** row: an installed application's, a
Steam title's, and one of the user's own games out of their ROM folder. It is
the same setting in all three, because what it sets is true of all three in the
same way — they are things with windows, and the row says how many pixels those
windows are drawn from.

**Native** is the top of the list and the answer everything runs at until
somebody says otherwise: the picture is the size of the screen it is on. Below
it are the sizes that fit that screen, largest first — 1600 × 900, 1280 × 720
and so on down. Choosing one is remembered, and the thing runs at it every time
from then on; choosing Native again takes it back. The highlight opens on
whatever is in force, so the row doubles as the answer to "what is this set to".

What it does is give the application a smaller picture and put that picture over
the whole display. A game asked for 1280 × 720 on a 4K television really does
draw 1280 × 720 pixels — a ninth of the work of a frame — and the compositor
enlarges the result to cover the screen. That is the opposite bargain from
[Application scaling](settings.md#application-scaling), which also configures a
window smaller but then asks the client to fill it with the screen's own pixels:
one is for a screen looked at from a sofa, and this is for a machine that cannot
quite keep up. Only one of the two is ever in force on a window, and this one
wins.

Because it is the compositor that carries it out, it works the same for all
three kinds of thing even though three different programs start them. It also
reaches windows under Xwayland, which application scaling deliberately does not:
there, magnifying a window that has already been drawn is a poor substitute for
a larger interface, and here it is exactly the request.

The list is the display's own. Only sizes that are exactly the shape of the
screen are offered, so nothing is ever pillarboxed by choosing from it, and a
screen no familiar size fits — an ultrawide, a panel on its side — is offered
three quarters, two thirds and a half of itself instead. What is remembered is
two numbers rather than a row on a list, so the same machine plugged into a
different television answers the question again from the same answer.

Two things follow from where the setting is *filed*. A game out of the ROM
folder is filed under the file it is, because every game in that folder is
played by the same emulator under the same window name — so the shell tells the
compositor which answer is in force on the way into each game's own launch. And
a Steam title is filed under `steam_app_<id>`, which is what Valve's launcher
calls the window it starts; a game that names its own windows instead is told a
moment after it opens rather than before, since nothing can know what a program
calls itself until it has said so once.

The panel is cut from the guide sidebar's glass — the same two pools of accent
light under one shallow refractive pane, and the same hairline over it — because
it is the same kind of object: a large quiet surface that things you can press
are laid on. The modal recipe the power dialog uses is deliberately not reached
for; that is a question about ending the session, and this is a note attached to
something you can still see behind it.

It takes every button while it is up. `B` closes it and goes back to whatever
raised it, one layer at a time; the guide button and the keyboard chord still
outrank it, and both put it away on their way past.

**Every one of them is drawn on a surface of its own**, a child of the display's,
with nothing else on it. There is one context menu in this shell at a time and
this is where it goes, whichever of the seven raised it — which is what lets the
compositor put it in front of a [floating window](settings.md#picture-in-picture), the
one thing otherwise drawn over everything the shell owns. A window somebody has
made large enough covers the very menu that offers to make it small again, and on
a pad, with no pointer to find an unseen row with, that is a dead end. The
surface's own input region is cut to the panel, so what the eye finds and what
the hand finds are the same shape, and nothing else about the panel — where it
is, how large, what shape — ever reaches the compositor.

The panel's glass shows what is behind it wherever that is, and it is the same
answer for all seven: what the shell drew itself it reads back out of its own
frame, its wallpaper it evaluates from the function that painted it, and what
another client drew it is handed a small picture of by the compositor — see
[What a pane of glass shows](architecture.md#what-a-pane-of-glass-shows).

