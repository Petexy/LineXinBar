# Architecture and rendering

[Documentation](index.md) · [Project home](../README.md)

- [lxb_shell_v1](#lxb_shell_v1)
- [Shell audio](#shell-audio)
- [What a pane of glass shows](#what-a-pane-of-glass-shows)
- [What draws, and when](#what-draws-and-when)
- [Architecture](#architecture)

## `lxb_shell_v1`

Layer-shell says nothing about either half of the problem above, so
[`crates/lxb-protocol`](../crates/lxb-protocol) defines a small private
protocol generated from one XML file for both sides:

**It is privileged, and the compositor enforces that.** The global is offered
only to the programs the session is made of — the shell the compositor started,
and `lxb-portal`, which draws its questions through the shell — and a client
that may not have it is not shown it in the registry at all; binding it by
number anyway is a protocol error. The compositor decides from the credentials
the kernel stamps on the connection when it is accepted, never from anything a
client says about itself.

The credential is the pid, and only ever the pid of a process this compositor
started itself and has not yet reaped. That is why `lxb-portal` is kept as a
real child and started again when it stops: a portal the *bus* activated has a
pid the compositor never learns, and one it cannot recognise cannot ask the
shell anything. What the pid is decidedly not is the *name* of the executable
behind it. Every program in the session runs as the user — Valve's client and
every game it starts, whatever the software hub installed, a browser — and any
of them can copy itself to a file called `lxb-portal`. A name is not a
credential, and this protocol hides windows, closes them, types, clicks,
captures the screen and logs the user out.

**The two are not given the same interface.** The shell may use all of it. The
portal may use `offer_kind`, `ask_to_pick_files` and `ask_to_share` — the three
requests that put an outside application's question to the user — and receives
the answers to those and nothing else. Every other request on a portal's object
is a `not_permitted` protocol error, and the state the compositor broadcasts
(what each window is, its title, the process behind it) is sent to shells only.

`--insecure-trust-program NAME` puts the old rule back for one session, for
working on the shell or on `lxb-portal --debug-pick` beside a running
compositor. It is named for what it is: any process of this user can take a
name, so a session started with it has this protocol open to all of them.

This used to be on wlr-layer-shell's footing — any client of the session — on
the reasoning that the compositor only runs what the user's own session started.
That reasoning was the mistake. A LineXinBar session deliberately runs a great
deal nobody here wrote or can vet: Valve's client and every game it launches,
whatever the software hub installed, a browser. The requests below hide any
application from the screen while leaving it running, close anybody's window,
inject key presses and pointer clicks, and end the session.

| | |
| ------------------- | ------ |
| event `guide`       | The compositor's guide binding fired. |
| event `keyboard`    | Its on-screen keyboard binding fired. Nothing else about typing goes over this interface — that is text-input, input-method and virtual-keyboard, all standard. |
| event `output_foreground` | Title of the topmost application on one display, empty when none. |
| event `foreground`  | The same for the session as a whole. Superseded; not sent from version 3. |
| request `close_output_foreground` | Ask that display's application to close. |
| request `close_foreground` | The same for the session as a whole. Superseded. |
| request `quit`      | End the session. |
| request `set_launch_output` | Name the display new applications should open on. |
| event `output_window` | One window on one display: a stable handle, its title and its logical size. A batch of them ends in `output_windows_done`. |
| event `output_window_app_id` | What one listed window's application calls itself, which is the only thing a running application can be recognised by. |
| event `output_window_pid` | Which process drew one listed window. The only thing about a window that is not a name somebody chose, and what lets a game be told from whatever else appeared while it was loading — see [Steam](steam.md#steam). |
| event `unseen_window` | One window the compositor is keeping off the screen on the shell's own instructions. A batch of them ends in `unseen_windows_done`. The counterpart of `output_window`, and separate from it because a hidden window is not on a display as far as anything else is concerned. |
| request `let_this_window_be_seen` | Let one window of a hidden application through without giving the application back — for a program the shell runs unseen that has stopped to ask something. See [Steam](steam.md#steam). |
| request `set_output_overview` | Enter or leave the window overview on one display, which is what draws the cards. |
| request `set_overview_selection` | Which card the shell is on, so the compositor scrolls the column the same way. |
| request `activate_window` | Raise and focus one window: how the overview doubles as a window switcher. |
| request `activate_window_from` | The same, flown in out of a rectangle — the tile an already-running application was pressed on. |
| request `kill_window` | End one window's application. Not a request it can refuse; see [the guide](guide.md#the-guide-overlay). |
| request `move_window_to_output` | Put one window on another display. |
| request `capture_window` | Photograph one window into a PNG at a path the shell chooses. |
| event `window_captured` | Where that picture went, or that it did not happen. |
| event `screenshot`  | The compositor's screenshot binding fired, and on which display. |
| event `volume`      | A volume key was pressed: one step up, one step down, or the switch that silences the session. A key held down arrives as a run of them. |
| request `capture_output` | Photograph a whole display — everything on it — into a PNG. |
| event `output_captured` | Where *that* picture went, or that it did not happen. |
| request `move_pointer` | Move the seat's pointer, as a mouse would — inside the application in front of the display being driven, and no further. |
| request `pointer_button` | Press or release one of its buttons. |
| request `scroll_pointer` | Scroll where it is, as a wheel or a touchpad would. |
| request `keyboard_key` | Press or release a key on the seat's keyboard — the arrows, not text. |
| event `output_app_id` | What the application on one display *is*, as opposed to what its title says. |
| request `set_output_hdr` | Drive one display in high dynamic range, and say how bright and how saturated. |
| event `output_hdr` | Whether a display can be driven in HDR, whether it is, and the peak it reports. |
| event `output_hdr_controls` | Which of the HDR settings that display can actually honour. |
| event `output_mode` | One mode a display can be driven at, with whether it is the current one and whether the display names it as its own. A batch of them ends in `output_modes_done`. |
| request `set_output_mode` | Drive one display at a different resolution and refresh rate. |
| event `output_transform` | Which way up a display's picture is drawn — and, by being sent at all, that this compositor is the one turning it. |
| request `set_output_transform` | Turn one display's picture, for a screen standing on its side. |
| request `set_output_application_scale` | How much larger than life the applications on one display draw their own interfaces. Not a magnification: each window is configured smaller than the screen and told to fill that with the screen's own pixels. `set_application_scale` beside it says the same thing for every display a shell has not named. |
| request `set_application_resolution` | How many pixels one named application draws its picture at, whatever display it ends up on. The opposite bargain from the scale above it: the window is configured smaller and the client is told *nothing* about scale, so it draws fewer pixels and the compositor puts that picture over the whole screen. `0×0` is the display's own size. |
| request `hide_pointer` | Take the cursor off screen, because the user has picked up the controller. |
| request `ask_to_share` | The desktop portal asking whether an application may see a display. |
| event `share_request` | That question, on its way to the shell — the only client that can draw it. |
| request `answer_share` | The shell's answer: a display, or nothing, which is a no. |
| event `share_answered` | That answer, on its way back to whoever asked. |
| request `offer_kind` | One kind of file an application will accept, ahead of the question that names it. Repeated once per pattern. |
| event `pick_kind` | That kind, on its way to the shell, in the order it was offered. |
| request `ask_to_pick_files` | The desktop portal asking for a file, some files, a folder, or somewhere to write. |
| event `pick_request` | That question, on its way to the shell — the only client that can draw it. |
| request `chose_file` | One file the user picked, repeated once each. |
| request `answer_pick` | The end of the question: whatever was named before it, and which kind was in force. Nothing named at all is a cancellation. |
| event `pick_chosen` / `pick_answered` | Those, on their way back to whoever asked. |
| request `cover_output_in_black` | Fade one display to black, or bring it back — the sheet [OLED protection](settings.md#oled-protection) rests a screen behind. It takes no input away, unlike the curtain the session goes out behind. Once it is all the way down, what is on that display is off screen: sent no frames and put to sleep until the display is asked back. |
| event `output_in_use` | Whether an application is in front of one display — anything the user opened, whatever started it — as against the shell being the whole of what is on the screen. |
| event `output_drawing` | Whether anything on one display has painted recently — which is what tells a screen that can be rested from one somebody is watching. |
| event `output_pointer` | The pointer is moving over this display. Sent on arrival and at most once every two seconds after, because the shell sees the pointer only where its own surfaces are in front. |
| request `set_picture_in_picture` | Whether a browser's picture-in-picture window floats over everything, how large it is drawn and which corner it sits in. One request for all three, because they are one rectangle. |
| request `set_window_floating` | The user's own word for whether one window floats, whatever it calls itself: a video told to fill the display it is in the corner of, and an application told to go and sit in that corner. One request in both directions. |
| request `set_menu_surface` | Which surface of the shell's a context menu is being drawn on, so that one surface can go in front of a floating window. Said only while a menu is up, and null again after. |
| request `ask_for_the_picture_behind` | Hands over a shared-memory buffer and asks the compositor to draw into it what it is compositing on one side of the shell's own surfaces — for the glass on them to refract. One ask, one picture. |
| event `the_picture_behind` | That buffer now holds it, and how much of it was drawn into. A size of zero means there was nothing to draw. |

`output_foreground` is what lets the menu say *Close KWrite* and notice when an
application it started has exited. It is reported per display because the
overlay belongs to one display: labelling it from the session's topmost window
offers to resume, or close, something on a screen the user is not looking at.

A window is attributed to the display it covers most of, rather than every
display it touches — otherwise one spilling over an edge claims both.

`capture_window` is a request because a Wayland client cannot photograph
another client's window, and the shell is a client: what it holds is a layer
surface of its own, so it cannot read even the application it is drawn over.
Only the compositor has those pixels. Where the file goes is the shell's to
decide, though, and is passed in — that is a question about the user's home
directory rather than about the display server — and `window_captured` always
answers, because a shell that has told the user it took a screenshot has to be
able to say where it went or that it did not happen.

`capture_output` is the same trade for a whole screen, and answers a different
question: not "what does this application look like" but "what is on this
display", which is a picture of the composite — wallpaper, windows, and the
shell's own bar or overlay over them, in the order they are drawn. It is taken
at as many pixels as the display is driven at and turned the way the display is
turned, and the pointer is left out of it: the cursor is drawn by the
compositor rather than being part of any surface, it is not on screen at all
while the session is driven from a controller, and an arrow burnt into a
screenshot cannot be taken back out.

`screenshot` is the round trip that makes a screenshot *key* possible. The
binding has to be the compositor's — the picture is of whatever is in front, so
a key that only worked while the shell had focus would never work over the game
somebody wanted a picture of — and the folder has to be the shell's, because
which folder that is depends on the language the account was made in. So the
key comes down as an event naming the display it was pressed on, and the path
goes back up as `capture_output`. The display then **flashes**, once, and only
after the picture has actually been written: it is the whole answer the user
gets, because a panel would be drawn behind the fullscreen application it was
reporting on, or in front of it with the keys taken off what they were doing.

`set_output_hdr` is a request rather than something the shell does itself
because neither half of HDR is a client's to touch: the metadata infoframe is a
connector property and the colour pipeline belongs to the CRTC. What the shell
owns is the decision. `output_hdr` comes back saying what actually happened,
which is what the Settings page reports — a shell that showed the switch as
taking effect when the connector refused would be the one setting in the shell
nobody could trust.

`output_hdr_controls` is separate from `output_hdr` because an event's
arguments cannot change once it has shipped, and it answers a different
question: not whether a display can do HDR, but which of the settings do
anything on it. It is what lets the page tell a control that is off from one
that has nothing behind it.

`set_output_mode` is a request for the same reason `set_output_hdr` is. A
connector's mode list comes off the hardware, changing it is an atomic commit
on a CRTC no client may touch, and everything on that display — every window,
every layer surface, the shell's own bar — has to be reconfigured around the
new size afterwards. `output_mode` carries the list rather than leaving the
shell to read it off `wl_output`, whose non-current modes are deprecated, and
it is what the page marks: a mode the hardware refused must not read as chosen.
A nested session reports no modes at all, because the size of its window is the
parent compositor's business and not this one's to change.

`set_output_transform` is a request for a different reason: turning is not
something the connector does at all. The picture is composited turned and
scanned out at the mode's own pixels, so no hardware can refuse it — but a
quarter turn swaps the display's logical width and height, which moves the
displays laid out beside it, re-arranges every layer surface anchored to it and
re-tiles every window on it. That is the compositor's, all of it.

`output_transform` is sent only for the displays this compositor turns itself,
which is what a shell needs and what `wl_output.geometry`'s own transform does
not answer: that one describes what a client should do about the output it is
drawing on, and it is advertised even for a display whose orientation belongs
to somebody else — a nested session, or a backend whose output carries a flip
of its own to compensate for the way it draws. A display absent from these
events is one the Orientation page leaves out rather than offers and cannot
honour.

`set_launch_output` exists because keyboard focus is the wrong thing to infer
the launch display from. Starting a second application from the guide hands
focus back to the *first* one long before the new window maps, so the new
window would join the old one's display. A shell drawing one surface per
display already knows the answer, so it says it outright — which also avoids
racing the round trip a focus change costs. The compositor treats it as a
preference and falls back to focus, then the pointer, then any output.

It is session-private and, unlike wlr-layer-shell, is not offered to ordinary
clients at all — see above. The shell degrades cleanly without it —
on another compositor it falls back to signalling the process group of what it
started itself, and `Quit` simply exits the shell.

## Shell audio

Every move that lands somewhere clicks, and a move that lands nowhere does not.
Pressing an edge is answered by nothing happening, which is why nothing sounds
there either. Which control made the move is not part of it: a click or a
finger on a row of the bar puts the selection there exactly as a direction
does, and the same move made by hand is owed the same answer.

The shell's two screens each have a voice, and each answers a move and a press
in it. The start screen moves with `press.ogg` and takes a press with
`press-selected.ogg` — a subcategory opened, a setting chosen, a search field
raised. The Home Button guide moves with `press-guide.ogg` and takes a press
with `press-guide-selected.ogg`. They are separate because the guide is a
screen of its own rather than another column of the start screen, and a user
who has looked away should be able to hear which of the two they are
driving.

Panels are not screens and do not follow the one they were opened over. A
context menu, a centred dialog and the mixer keep the same voice wherever they
were raised, because a component that changed its sound with its backdrop would
be two controls that look alike. Raising the guide from an otherwise empty
start screen also leaves its background music playing; raising it over an
application does not start that music above the application.

Leaving a lattice subcategory is `press-back.ogg`, whether Left or Back walks out
of it or a pointer or finger presses the visible trail or category row. One
gesture makes one sound even when a trail press crosses several levels: the
sound answers the decision to go back, not every column it passes.

A key of the on-screen keyboard going down is `keyboard-click.ogg` rather than
either screen's click, because the board is its own instrument: putting a key
*down* is the thing that only happens there, while walking across its keys is
walking a bar and still sounds like one. Every key of it, including Shift and
the key that puts the board away, because a board where two of the keys
answered silently would read as a board with two dead keys on it.

An application starting from the start screen is `app-launch.ogg`, and it is
the one sound here that is not a click, because it is not an acknowledgement:
the press has already been answered by the splash growing out of the tile, and
what this one says is that something is on its way. It belongs to that screen,
and only to it: a tile pressed on the start screen sounds it whether the shell
forks for it or comes back to a program that is already up, because the tile is
where an application is *started* from. The guide never sounds it. Every press made on the overlay
is answered in the overlay's own voice, the ones that hand an application the
display included — Resume with something running behind it, and a window card
chosen out of the deck, both `press-guide-selected.ogg`. A screen with a voice
of its own does not borrow another's for half of its rows. A press that failed
to start anything sounds nothing at all.

`guide-open.ogg` belongs to a *button* rather than to a screen, and it is the
only one that does. The guide button works from inside anything — a game holding
every other key, a panel, the on-screen keyboard — so it is the one press whose
answer cannot be "look at the screen and see": it may have been pressed exactly
because what is on the screen has stopped listening. It sounds when that press
*opens* the overlay and at no other time. Closing it is silent, because the
overlay leaving is the screen behind it coming back. So is the guide arriving by
any other route — a portal's question about sharing a screen, an authorisation
panel raised over a game — because neither of those is somebody asking for the
guide.

`screenshot.ogg` is one of the two sounds here that answer something the shell
*did* rather than something the user pressed. It is the pair of the white flash the
compositor draws over a display that has just been [photographed](desktop-integration.md#screenshots),
and it sounds at the same moment for the same reason: the chord is often spelled
with the user's eyes on the game, and an acknowledgement only one sense can
reach is one half of an acknowledgement. Like the flash it waits for the file to
be on the disk, so it says a picture exists rather than that a chord was
spelled, and a capture that failed sounds nothing. The picture of a single
window does not take it either — that one is answered by a panel naming the
folder, on a screen the user is already looking at.

`polkit.ogg` is the other, and it answers the one panel in the shell that nobody
asked for: an [authorisation prompt](desktop-integration.md#authorisation-prompts), raised over
whatever was in front of the user because a program somewhere wants a password.
Everything else the shell says is a reply to a control that was just pressed;
this one has to announce itself, or a question that has taken the screen arrives
in silence for anybody who happened to be looking at the room. It sounds as the
panel goes up and only if it went up, and a password refused does not sound it
again — that is the same question still waiting, not a new one.

The start screen has the eleventh recording, `start-bg-music.ogg`. It belongs
to the session rather than to a screen: it loops while every display is showing
the start screen and nothing at all is open, and it fades as a launch or a
returning window begins taking any of them. One application anywhere ends it —
a game on the first display and the start screen on the second is a session
with a game in it, and crossing to that second screen must not start music up
behind the game. When the
last application closes, the shell constructs a fresh stream at sample zero —
even if the previous one is still fading — so coming back never resumes halfway
through the track.

It is also the one recording that can be turned off outright, from [`Settings >
Sounds > Start music`](settings.md#sounds), which is where the rest of that is written down.
The ten short clips cannot: each of them answers a control that was pressed or an
event that arrived, and a press with no answer reads as a button that does not
work. A background is the sound the shell makes at somebody who has pressed
nothing, which is exactly what a person reading in the same room may not want.
Turned off it stops the way muting stops it — at once, and without the fade an
application gets — and turned back on it begins again from the beginning.

All eleven recordings are shipped in the repository beside the shell's source
for the same reason the fonts and the cursor are: there may be no desktop on
the machine, and so no theme of sounds to borrow one from. The ten short
effects are decoded once at start-up, and every press is its own sound rather
than one restarted, so a held D-pad walks the bar to a run of clicks rather
than to one click held at the point the repeats overtook it. The much longer
music stays compressed and is decoded as it plays instead of occupying a
whole decoded track's worth of memory.

What no press does is lay a sound on top of a copy of itself. Identical
recordings add, and being identical they add in phase, so a spun wheel used to
answer with one click many times its own height: a single scroll event can be
worth several rows, every row it crosses clicks, and a dozen arriving together
took the output to full scale. A click asked for within a sixteenth of a second
of the last of its kind is dropped rather than mixed. Two clicks that close are
not two sounds anyone can hear apart, and the gap is shorter than the repeat of
a held direction, so a run of clicks is still a run of clicks.

The `System` row of the volume mixer in the guide sets how loud all of this is —
the shell's effects and background music, and the one row on that panel no
sound server knows about. Left and Right move it, `A` silences it, and the
click the direction makes is heard at the level it has just been moved to, so
the row previews itself. Muting also drops the music rather than advancing it
silently; unmuting on the start screen begins it again. The setting is written
to
`shell.toml` as `sound-volume` and `sound-muted` at every step rather than when
the user stops moving it: the shell is idle between presses, and a level nobody
wrote down is the one a machine switched off at the wall would lose.

Playing is in-process, unlike everything else the shell does with audio. The
volume bars in the guide shell out to `wpctl`, `pactl` or `amixer`, because
what they set is what the *session* is doing; shell audio is one more stream
playing into that session, its effects have to start within a frame of the
button, and a process spawned per step would be a fork every 90 ms under a held
direction.
The output is opened through ALSA, which is PipeWire or PulseAudio on a machine
that has one and the sound card itself on a machine that does not. A session
whose sound server is not up yet is retried a few seconds later; if an open
output disappears, the next loop drops the dead stream and opens the current
default. Start music is rebuilt there from sample zero if the lattice still owns
the display. A machine with no output at all is silent, and nothing else about
the shell changes.

## What a pane of glass shows

Every pane in this shell refracts what is behind it, and "behind it" is three
different things it reaches three different ways.

What the **shell itself** drew is read straight back: the frame is built in an
offscreen texture and a pane samples a snapshot of it, so a button refracts the
panel it is resting on and a panel refracts the icons under it. What the shell
draws on *another* of its surfaces is read the same way, at full resolution —
that is how a context menu, which has a surface to itself, still refracts the
start screen behind it.

The **wallpaper** is not read at all: it is *evaluated*, from the same function
that paints it, wherever the snapshot is transparent. That is sharp at any size
and costs nothing, and it is why a game's key art — which is drawn as the
wallpaper rather than as a layer over it — refracts correctly too.

What **another client** drew, the shell can do neither with. A game's window, or
the video in a floating one, is the compositor's pixels and this shell never sees
them. So the compositor draws them and hands them over: `lxb_shell_v1`'s
`ask_for_the_picture_behind` gives it a shared-memory buffer, it draws what it is
compositing on one side of the shell's own surfaces into it, small, and says when
it is done. One ask, one picture — a display whose shell is not drawing asks for
nothing, so a game in front pays for none of this.

Small on purpose. A pane frosts what it transmits — it samples several rungs down
a blur chain — so what it wants back is something already blurred, and 256 pixels
along the longer edge is a readback a shell can afford every frame. It is one
frame behind, which is 16 ms of a picture about to be frosted past recognition.

And it is **absorbed on the way in**. Everything here is designed against a
wallpaper that is deliberately dark, which is what makes a pane's tint thin
enough to be worth seeing through. A film or a game let through at full strength
turns the same pane into a window: over a bright frame its own colour disappears
and the white text on it stops being readable, which on a menu is a control that
cannot be answered. Tinted glass absorbs what it transmits, and it absorbs the
same amount whatever is behind it, so the material reads the same everywhere.

## What draws, and when

A frame callback is how a Wayland client is told to draw its next frame, and
LineXinBar hands them only to what is actually on screen. Every application is
tiled across its whole display, so a window with another one in front of it is
not partly visible — it is not visible at all, and it stops at no frames until
it comes back to the front. A video left playing behind a terminal decodes
nothing.

This is decided from the stack rather than from what shows through it. A
translucent application in front changes what the user can see of the window
behind, not whether that window is worth drawing, so it goes quiet either way.
Anything the front application only partly covers keeps drawing: a bar
reserving space at the edge of the display tiles the application into what is
left, and a window standing out past that has pixels of its own on screen.

The guide menu is the exception. Its overview shows every window on the
display at once, and those cards are the live windows rather than screenshots,
so opening it starts them all again and closing it puts them back to sleep.

Layer surfaces are never held back, covered or not. The shell is one, it
presents FIFO — which paces on exactly these callbacks — and it runs a single
thread, so a shell denied them blocks inside its own present instead of
idling, never hears the guide button, and takes the session with it. It
already stops drawing by itself once something covers it, so there was nothing
to reclaim there. Applications are different: one blocked in its present is
one costing nothing, and it wakes the moment it is back in front.

The shell starts drawing again for the four things it puts in front of an
application: a launch splash, the on-screen keyboard, a bubble in the corner
and the control a volume key raises. It then draws *only* that thing — the
start screen belongs behind the application, and a shell that drew both would
lay every icon of the bar over the game underneath.

X11 applications are a partial exception in the other direction. Xwayland
absorbs the missing callbacks rather than passing the stall on, so an X11
client can carry on rendering into a window nobody can see; what is saved
there is LineXinBar's compositing, not the client's drawing. Wayland-native
applications stop properly.

## Architecture

```
crates/lxb-compositor/
  state.rs        global state, split so a render pass can borrow the
                  backend and the compositor state at once; session shell
  handlers.rs     Wayland protocol handler implementations
  config.rs       config.toml, and the defaults a missing one gives
  outputs.rs      multi-display layout: positions, scale, transform, tiling
  input.rs        input routing, focus policy, keybindings
  focus.rs        common Wayland/X11 keyboard, pointer and touch targets
  cursor.rs       the compositor-drawn cursor and its XCursor theme
  text_input.rs   text-input, input-method and virtual-keyboard, for the
                  on-screen keyboard
  shell_control.rs  compositor half of lxb_shell_v1
  overview.rs     the windows of a display animated into the guide's cards
  restore.rs      one window flown back out of the tile that asked for it
  teardown.rs     ending an application, as against ending a process
  capture.rs      photographing one window, or one whole display, into a PNG
  flash.rs        the white a display gives when it has just been photographed
  blackout.rs     the black one display rests behind while another one is
                  being used — see OLED protection
  screencopy.rs   wlr-screencopy: what the portal is built on, and offered to
                  the shell and the portal alone
  hdr.rs          the connector's metadata and the CRTC's colour pipeline
  xwayland.rs     private XWayland server's X window manager and selections
  render.rs       render element assembly, shared by every backend
  backend/
    winit.rs      nested, one window
    x11.rs        nested, one window per virtual output
    udev.rs       DRM/KMS, libinput, libseat, multi-GPU

crates/lxb-protocol/
  protocols/      lxb-shell-v1.xml, the single source for both sides
  lib.rs          wayland-scanner bindings, client and server behind features

crates/lxb-desktop/
  apps.rs         .desktop parsing and Plasma-style categorisation
  appinfo.rs      what installed an application, and what that says about it
  uninstall.rs    one Origin translated into one argv, and whether it may run
  steam.rs        Steam as the shell holds it: one account, one sign-in
                  panel, one column
  polkit.rs       the session's polkit agent: polkitd on one side, PAM's
                  helper on the other, and the panel in between
  secret.rs       a password, from the key that types it to the pipe that
                  consumes it
  icons.rs        icon theme lookup, PNG/SVG rasterisation
  theme.rs        the palette: twelve accents, and every colour read as it is drawn
  settings.rs     the Settings column, written here rather than found on disk
  model.rs        the shared catalogue, and one cursor per display
  controller.rs   gamepads through gilrs, and what a button means
  pad_guard.rs    the pad taken away and given back with the guide button
                  missing, so that no application can read it
  steam_hid.rs    the second-generation Steam Controller, read from hidraw
  guide.rs        the overlay's modes and menu
  menu.rs         the context menu: entries, selection, scrolling and
                  growth, with no idea what raised it
  dialog.rs       the centred panel: a question that has taken the screen over
  launch.rs       the splash between pressing A and the application being there
  media.rs        the walk over $HOME for music, films and photographs,
                  what order the rows are in, and what opens one
  thumbs.rs       a frame of the film, a photograph scaled down, and the
                  freedesktop cache both are kept in
  trash.rs        the freedesktop trash, for the Delete row
  transfer.rs     the folder a file is carried to, walked as a mirrored bar,
                  and the copy or move itself; a name is changed in main.rs,
                  beside it, because it is one `rename` and a field
  screenshot.rs   where a screenshot goes, in the language the account was made in
  pointer.rs      the right stick as a mouse, and which applications it is
                  turned on for
  keyboard.rs     the on-screen keyboard: its keys, the input method and
                  virtual keyboard behind them, and the grab that lets a
                  real keyboard drive it
  system.rs       volume, per-application volume and brightness, off the
                  main thread
  power.rs        what is left in the battery, out of the kernel's own
                  power_supply directory, and which supplies are this
                  machine's rather than a peripheral's
  volume.rs       how long the control a volume key raises stays on screen
  sound.rs        the ten effects and Start music, and the output and focus
                  transitions they go through
  ui.rs           layout: model to quads and text runs
  gpu.rs          wgpu renderer, one atlas and two pipelines
  shaders.wgsl    animated backdrop, instanced quads
  offscreen.wgsl  the blur the glass reads through, and the copy to the display

crates/lxb-steam/
  lib.rs          the worker thread, and the two channels the shell holds
  auth.rs         signing in: IAuthenticationService, both ways round
  library.rs      what the account owns, and what of it is on this disk
  client.rs       Valve's client as a background process: where it is,
                  whether it is up, whether it has signed in
  webui.rs        the calls this shell makes into the client's own
                  interface — signing it in, and moving a game on or off
                  the disk — and why they are made there
  session.rs      what survives a reboot, and what must never be written down
  protobuf.rs     the wire format Steam's services speak, written out by hand
  vdf.rs          Valve's key-values, which is what the disk answers in
  rsa.rs          encrypting the password under the account's own key
  password.rs     it, from the shell's field to that encryption
  base64.rs       the two spellings Steam uses
  web.rs          one HTTPS agent, and the two shapes of call made over it
  qr.rs           the code on the screen, as squares for the shell to draw

crates/lxb-portal/
  cast.rs         one display, going out as a PipeWire stream
  screencast.rs   org.freedesktop.impl.portal.ScreenCast, over D-Bus
  consent.rs      who may see the screen, and which one
```

