# Guide overlay and context menus

[Documentation](index.md) · [Project home](../README.md)

- [The guide overlay](#the-guide-overlay)
- [The context menu](#the-context-menu)

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

### What is coming down

The far corner from the column, and the one thing here that is not part of the
menu: while a Steam game is downloading, a card stands in the bottom right
wearing the game's own icon, with what is arriving, a bar, and the percentage.
It is a reading and not a control — no selection stops on it and no press
reaches it — and it is drawn only while this menu is open. It is described with
the rest of the download in [Steam](steam.md#steam), where the numbers on it come from.

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
| `A`, or R3                 | Left click |
| `B`, or L3                 | Right click |
| Left stick, or the D-pad   | Scrolls what is under it |

Two spellings for each button, because two hands reach for different ones: `A`
and `B` are where a hand goes without being taught, and the stick presses are
where a thumb already is while it is aiming. Nothing else is taken — the
shoulders still move between displays, and the left-hand face button is still
half of the keyboard's chord.

`A` can be a click at all only because it is not anything else at the time: the
shell drops every action but the guide button and the keyboard chord once an
application is in front of it, so `A` is doing nothing there. Back on the
shell's own screens it is Launch again, because there the pointer is switched
off.

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

The stick goes back to being a stick the moment one of the shell's own screens
is in front of the application — the guide menu, or a launch still waiting for
its window. There is nothing on either to point at, and the pointer stays
exactly where it was parked.

The on-screen keyboard is the one thing drawn over an application that does
*not* take the whole pointer away, because it does not need to: it is driven
with the D-pad, the left stick and `A` — the clicks and the scrolling — and has
no use at all for the right stick. So the two halves part company for as long
as it is up. Aiming carries on underneath the board, and the pointer is still
where it was left when the board goes away; `B` puts the board away, and hands
the buttons back with it.

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
| The bar | The application on the focused tile, out of the disc it stands on | Information, Uninstall / Launch, Close |
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

