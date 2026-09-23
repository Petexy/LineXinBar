# Controls and multiple displays

[Documentation](index.md) · [Project home](../README.md)

- [The on-screen keyboard](#the-on-screen-keyboard)
- [Mouse and touch](#mouse-and-touch)
- [The cursor](#the-cursor)
- [Keyboard focus](#keyboard-focus)
- [Default keybindings](#default-keybindings)

### Several displays

Each display gets its own bar, not a copy of one: they browse independently and
remember where they were. Only one takes input at a time — the others are
dimmed and drop the footer, so it is obvious which one the controller is
driving. `L1` / `R1`, or `Tab` / `Shift+Tab`, hand control to the next display,
and a click on a display takes it there; the compositor's first output has it at
startup, since Wayland has no notion of a primary display. Nothing else moves
it — a pointer merely crossing onto the other screen leaves control where it
is, because a mouse on its way somewhere else is not a decision about which
screen the user is using.

An application launched from a display opens on that display, because the shell
names that display before starting anything. It does not leave the compositor to
work it out from keyboard focus: focus has usually moved on to something else by
the time the new window maps.

It names it twice, and the two answers are different questions. `place_launch`
files that one press — the display it was made on, the process the shell forked,
and the class the window will carry — so the window it turns into opens where it
was pressed whatever else has been started since. Two screens can be loading two
things at once, and a single answer for the session named one of them: the two
windows arriving in either order both went wherever that one answer happened to
point, which put a game on the screen nobody had pressed it on and left the other
screen waiting out its patience for a window standing next door. `set_launch_output`
is still sent, and is now the fallback for a window that answers to no record.

`set_driven_output` is the other one, and it is about the person rather than
about a window: which display hands the keyboard back when something closes,
which display's game is the X screen's active window, and which display a
screenshot binding photographs. It was the same field until the two came apart,
and read as "where the user is" the launch answer walked all three off to
whichever screen was loading — for as long as it took to load. A game being
played on the other screen was then told by the root window that it was in the
background, which every Proton title reads.

And it stays there. The display a window is placed on is recorded on the window
itself, so every later re-tile — a sibling window closing, the client asking to
be maximized or fullscreened, an X11 client moving itself — puts it back on the
same screen. Nothing moves a window between displays but a user asking for it:
`Super+Shift+→`, or the two move rows in [the menu](guide.md#the-context-menu) over its
card in the guide. Without that record the placement would be re-derived from
the window's bounding box, which a window that has mapped but not yet drawn does
not have: an application whose
updater or splash window closes as the real one appears would arrive on the
first display instead of the one it was started from.

Only the catalogue of applications and the running processes are shared. The
device, icon atlas, shaders and shaped-glyph cache are shared too, so a second
monitor costs one more swapchain rather than a second copy of everything.
Hotplug is followed in both directions; unplugging the display that had control
passes it to a neighbour.

| Input                                      | Action                              |
| ------------------------------------------ | ----------------------------------- |
| `←` / `→`, D-pad or left stick left/right  | Change category                     |
| `↑` / `↓`, D-pad or left stick up/down     | Change row                          |
| `Enter`, controller `A` or `Start`          | Launch, open a subcategory, or choose a value |
| `→` inside a subcategory                    | Open the subcategory under the cursor |
| `←` inside a subcategory                    | Step back out one level             |
| `Tab` / `Shift+Tab`, `L1` / `R1`            | Move to another display             |
| `Alt+Tab` / `Alt+Shift+Tab`                 | Walk [the guide's cards](guide.md#the-cards) while Alt is held; letting it go switches to the one the walk landed on |
| `Esc`, `Backspace` or controller `B`        | Step back out one level, and nothing at the top of a column |
| `Home`, `Super`, mouse side button, controller Guide/STEAM button | Open the guide overlay |
| `Y`, `F10`, right mouse button, controller `Y`/`Triangle` | Open [the context menu](guide.md#the-context-menu) on what is selected |
| `Shift`, controller `X`/`Square` | Open [the friends list](steam.md#who-is-on-steam-and-talking-to-them) down the right of the screen |
| `P`, controller right stick pressed, with the guide open | Hand the guide's directions to the [videos floating over it](settings.md#picture-in-picture), and hand them back |
| `Print` (with anything held), `Ctrl+Shift+3`, `Alt+Shift+3`, controller Guide/STEAM + `R1` | [Photograph](desktop-integration.md#screenshots) the display being driven |
| Controller Guide/STEAM + Select/View | Ask [Valve's own overlay](#the-guide-button-is-the-shells-alone) to come up over the Steam game in front |
| The volume keys, with anything held | Turn [the session](guide.md#quick-settings) up or down a step, or silence it |

Keyboard navigation also accepts the keypad arrows, WASD, and HJKL. Held
directions repeat after a short delay; the analogue stick uses a dead zone
with hysteresis so drift near its edge cannot rapidly change selection.

Controllers are discovered and hot-plugged directly through the Linux gamepad
API, using SDL-compatible mappings (including `SDL_GAMECONTROLLERCONFIG`, as
used by Steam). A pad the mapping database does not know still works: buttons
it cannot name fall back to their raw Linux codes, so `BTN_SOUTH` launches and
`BTN_EAST` goes back whatever the database thinks.

The two middle face buttons are the awkward case, because the naming that comes
back for an unmapped pad is positional rather than physical — the same
`Button::North` is the button with `X` on it under one driver and the one with
`Y` on it under the next, and the raw code says no more than the name does. The
two of them want different screens — the friends list and the context menu —
so the pair is split by the raw code read at the *legacy* gamepad names' word:
`0x133` is the left-hand button and `0x134` is the top one, which is what xpad
and every driver modelled on it send. That is a guess, and on a pad that both
numbers the pair the other way round and is missing from the database it is the
wrong way round; there is nothing else to ask. Select held with the left-hand
one is still the keyboard chord. A pad the database does know is read by name
only, and the guess never reaches it.

One pad escapes the gamepad API entirely. The second-generation Steam
Controller has no kernel driver — `hid-steam` claims the original, its receiver
and the Deck, and this one falls through to `hid-generic` — so it has no
joystick node at all and the gamepad API enumerates nothing. It is read from
its hidraw report instead, which is the only source that survives both of the
states it has: on its own it is in the firmware's lizard mode, pretending to be
a keyboard and a mouse, and the moment Steam is launched Steam claims it and
writes lizard mode off. Reading hidraw is not exclusive, so it works alongside
Steam's own reads, and it is opened read-only — leaving lizard mode means
*writing* feature reports, which is Steam's business. The compositor drops the
pad's lizard keyboard so the same button cannot arrive twice.

Every button press is logged at debug level with its mapped name, its raw code
and which of the two namings it came under, which is the fastest way to work
out what an unusual pad is actually sending:

```sh
RUST_LOG=lxb_desktop::controller=debug lxb-desktop
```

Controller initialisation failure is non-fatal and leaves the keyboard usable.
Pass `--no-gamepad` to skip controller discovery entirely — which also turns
off [the stick pointer](guide.md#the-stick-pointer), since there is no stick to read,
and leaves every pad's guide button alone along with the rest of it.

### The guide button is the shell's alone

The compositor holds every other spelling of the guide back from the focused
application outright — the `Home` key, `Super`, the mouse's side button, both
edges of each. It is the way *out* of an application that is holding everything
else, and an application that could take it over would be an application there
is no way out of.

A controller cannot be held back that way, because a controller never passes
through the compositor at all. There is no gamepad protocol in Wayland, so a
game opens `/dev/input` itself and sees exactly what the shell sees. So the pad
is taken away and given back with one button missing: LineXinBar grabs each
pad's device node — `EVIOCGRAB`, which makes every other reader of it deaf —
and puts a `uinput` device in its place that says it is the same pad and
repeats everything it does except the guide button. Applications find the
stand-in where they would have found the pad, and the only reader of the real
one is this shell.

The stand-in is the same pad in every respect an application can ask about:
name, bus, vendor, product, version, every button, every axis with its range
and resolution, and force feedback, which is passed back the other way so a
game can still shake the real pad. The guide button is *declared* there and
never sent — SDL builds its controller GUID from the identity and numbers
buttons by walking the capability bitmap, so a stand-in that differed in any of
that would be a pad the mapping database has never heard of, with every button
in the wrong place.

Three rules keep this from costing more than it is worth:

- **A pad is never taken without being given back.** The grab and the stand-in
  are made together, and if any part of it fails — no `/dev/uinput`, no
  permission, a node that never appears — the grab is dropped and the pad is
  left exactly as it was found. Losing the rule is a leaked button; losing the
  pad is a console nobody can play.
- **A pad another program has already grabbed is left alone**, for the same
  reason.
- **Nothing that can type is ever taken**, however many gamepad codes it also
  declares, and nothing `uinput` made — which is this shell's own stand-in,
  Steam Input's pad for a game, or another session's.

The one route this cannot cover is `hidraw`. A pad that speaks HID has a raw
report node too, reads of it are not exclusive, and there is no kernel
interface for making them so — SDL will read a pad that way in preference to
`/dev/input` when its HIDAPI drivers recognise it. So every application the
shell starts is handed `SDL_HIDAPI_IGNORE_DEVICES` naming *the pads this shell
is standing in front of*, and only those: a listed pad still arrives complete
through `/dev/input`, and a pad with nothing standing in for it is never
listed, so nothing is ever asked to ignore the only route a controller has.
Anything already in the environment is added to rather than replaced.

**Valve's client is handed a shorter list than everything else**, and the
difference is one pad. A controller the guard is holding is on both lists: the
client reads `hidraw` around a grab like any other program, and the stand-in is
waiting for it on `/dev/input`. A controller with *no kernel driver* — the
second-generation Steam Controller, below — is on neither the client's list nor
anybody's road to it but this shell's, because there is no `/dev/input` node to
fall back to. Naming it to the client does not move the client onto the
stand-in; nothing moves the client onto anything. It takes the pad away from
the client altogether, and from every game the client launches. Measured on the
pad this was written for: `SDL_hid_enumerate` returns five interfaces of
`28de:1304`, none at all with the id on the list, and the client's own log goes
from five `Local Device Found` lines to none.

#### The pad with no driver at all

The second-generation Steam Controller reaches the same place by the opposite
road. `hid-steam` does not claim it, so the kernel makes it no gamepad node —
there is nothing to grab, and nothing for a game to find either. LineXinBar
drives that pad itself: it reads the pad's report off `hidraw` and **builds the
gamepad the kernel did not**, a `uinput` device of an Xbox controller's exact
shape, so that SDL, GilRs, RetroArch and any game find a complete controller
where they look for one.

The Steam button never reaches that device. It is declared on it, for the same
button-numbering reason as above, and it is the one control the driver keeps —
which needs no grab to enforce, because a stand-in this shell builds is only
ever told what this shell chooses to tell it. The pad's own raw node joins the
ignore list handed to *applications*, now that there is somewhere else to read
it — and stays off the one handed to Valve's client, which is the other half of
this pad's road to a Steam game.

**Valve's client is the one reader that cannot be shut out**, and it is worth
being plain about why. Steam reads this pad's raw HID reports with its own code
rather than through SDL, so the ignore list above does not reach it, and a raw
node cannot be held exclusively by anybody — there is no `EVIOCGRAB` for
`hidraw`. Every other controller escapes this by having a device node to take
away; this one has none, which is the whole reason the shell had to build it
one. So the button reaches Steam whatever the shell does.

What is left is to ask Steam not to act on it, and Steam has a setting for
exactly that — *Guide button focuses Steam*, on its Controller page. The shell
turns it off through the same interface it signs the client in through, every
time it wakes the client, because the client keeps that setting in memory
rather than on disk. Without it, every press of the guide button opens Big
Picture behind the shell. Measured three presses either way: with the setting
on, Steam goes to Big Picture on the first press; with it off, its window list
does not move.

**And what the shell takes, it gives back as a chord.** The guide button is the
button Steam's overlay comes up on everywhere else a machine is shaped like a
console, so taking it and offering nothing in its place would be this shell
deciding that nobody may reach Steam's friends list, its browser or its guides
while a game is running. **Guide + Select** — `STEAM`+`View` on a Steam
Controller or a Deck, the PlayStation button and Create on a DualSense, Guide
and View on an Xbox pad — asks for the overlay over the game in front. The two
middle buttons, side by side, reachable with one thumb, which matters more here
than it does for the screenshot chord: this one is pressed while playing.

There is nothing to *ask* for it. Valve's whole client interface was read off a
live build, and the overlay is only ever something the client is told about —
`SteamClient.Overlay` registers for activation requests and reports state, and
nothing in it raises one. The overlay is not the client's to raise: it lives
inside the game, in the library Steam preloads into it, and what that library is
watching for is a keystroke. So the shell sends the keystroke, over
`lxb_shell_v1.keyboard_key`, which puts it on the seat's own keyboard exactly
where a real Shift+Tab would land.

Which keystroke was read off the same live client rather than assumed:
`overlay_key` is `Shift+Tab` — keysym 65289 is `XK_Tab` — and
`gamescope_guide_hotkey`, the same question asked for a machine that *is* shaped
like a console, is the same two keys again. They go out as Linux key **codes**,
which is what the protocol carries, and here the two cannot disagree: Tab and
Shift are in the same place on every layout xkb has, which is a large part of
why Valve could pick them.

Two refusals, and both are silent. Nothing but a game Steam started may be sent
it — in a browser the same keystroke walks the focus backwards, and a chord that
did something different in every application is a chord nobody can press with
confidence — and the shell must not be holding the keyboard itself, because then
the key would be delivered to the shell. Neither is worth saying on screen: the
first is the ordinary state of a machine with no game running, and a message
every time somebody brushed two buttons in a menu would be worse than nothing
happening. The third failure *is* said, because it is the one nobody could work
out alone: a client that has moved its overlay to another key, or switched it
off altogether, is read on every wake and written to the log — the shell sends
Valve's default, and a moved overlay would make the chord silently dead.

The triggers are analogue over the pad's own fifteen bits rather than the
single byte `xpad` gives one — every reader scales by the range a device
declares, so declaring the true one costs nothing and keeps the part of a
trigger that is not a button. The stand-in has no rumble: shaking the pad means
writing to it, and the driver does not.

The driver is deliberately **read-only**. Leaving lizard mode, rumbling, the
gyro and the trackpad haptics all mean *writing* to the pad, which is what
Steam does when it claims it, and two programs configuring one controller is
one controller doing neither. Reading only is what lets the shell and Steam
hold the same pad at once — so with Steam running the trackpads, the gyro and
the haptics keep working through Steam, and with Steam closed the trackpads
still move the pointer as the firmware's own mouse. The lizard-mode *keyboard*
that would otherwise deliver every button a second time is dropped in the
compositor.

The shell reads this pad from its report and not from the gamepad it made: the
report's layout was captured on the hardware, so which button is which is known
exactly, where a mapping database asked about a pad it has never heard of gets
two of the face buttons the wrong way round. Everything else on the machine
reads the gamepad.

Icons are resolved through the freedesktop icon theme spec, following
`Inherits` from `index.theme` and falling back to hicolor and
`/usr/share/pixmaps`. Both PNG and SVG are supported, including extensionless
absolute paths used by AppImages and non-standard theme directory layouts.
Broken high-priority theme entries fall through to inherited themes, and a
generic application icon is used when a desktop entry provides no usable icon.

## The on-screen keyboard

A console has no keyboard, so the shell is one. It comes up by itself when an
application says a text field has the cursor, and can be summoned by hand at
any time — over an application that never said so, or over the bar with nothing
running at all.

| Control | Effect |
| ------- | ------ |
| D-pad / stick | Move between keys |
| `A` | Press the key under the cursor |
| `B` | Put the keyboard away — as does the key at its bottom right |
| Select + the left-hand face button | Show or hide it, anywhere |
| `Super+K`, `XF86Keyboard` | The same, from a keyboard |
| Any key on a real keyboard | Puts the board away, and is typed into the application |

Where [the stick pointer](guide.md#the-stick-pointer) is on, the right stick and
the two triggers keep working underneath the board: it wants none of them, and
the field about to be typed into usually has to be clicked on first.

The left-hand face button is `X` on an Xbox pad, Square on a PlayStation one
and `Y` on a Nintendo one — the same button in all three cases. Select is
likewise Back, View, Share, Create or `−` depending on whose pad it is, which
is why the corner hint draws both rather than naming either.

Select does nothing by itself: it is a modifier, and only a modifier. A
modifier that also opened the menu would make the chord two things at once —
the menu would appear behind the keyboard, and since the menu closes the board,
the face button after it could only ever be opening one afresh, so the chord
could show the keyboard but never hide it. The menu keeps the guide button,
`B` from the bar, and the compositor's own binding.

It rises through the display's bottom edge rather than fading in, and leaves
the same way, on the same quarter second. A keyboard that appeared on the spot
over a running application reads as the application having done something; one
that slides up reads as the shell putting it there — and one that vanished on
the keystroke dismissing it would read as the shell having lost it. What is
kept is a *position* rather than the moment it opened, so the two directions
are one movement: a board dismissed halfway through arriving falls back from
where it is, and one summoned again while it is still leaving comes up from
there. The keys go back to the application on the instant either way — a
quarter of a second unable to type would be the board's parting insult — so
what is still on screen during the fall is a picture and nothing more, and
clicks pass straight through it.

The guide is the one exception, and it does not slide: the menu is not this
screen with the keyboard taken off it but another screen, drawn over the whole
of the space the board would have been travelling through.

### What a real keyboard does to it

It dismisses it. The board takes the physical keyboard for as long as it is on
screen, and the first key pressed on that keyboard is the answer to the only
question the board was ever asking: nobody hunts for letters with a stick while
a keyboard is under their hands. So the board leaves, and the key goes on to
the application as though the matching key on the board had been pressed, with
whatever modifiers were held — Ctrl+C still copies, Backspace still deletes,
and the letter that dismissed the board is the first letter of the word. It
matters most where the board comes up on its own: a search box taking the
cursor should not cost the user a keystroke, nor leave them looking past a
keyboard they are not using.

Nothing on the board is navigable from a keyboard, deliberately. Arrows that
moved a cursor around a picture of the keys already under the user's hands
would be six keys spent on the one thing a keyboard makes pointless. Escape is
the one key that dismisses without typing, which is what it does on every
keyboard that appears by itself.

Having been dismissed that way, the board stops offering itself: the next text
field gets the corner hint rather than a keyboard, because the user has already
said once that they can type, and being told again at every text box is the
thing they dismissed. The controller shortcut still summons it — for the
evening the keyboard is put down and a pad picked up — and summoning it is what
forgets the refusal.

None of this is done by taking keyboard focus, which the board must never do —
focus is what carries text-input focus, so the moment the shell holds the keys
the application's field deactivates and the board puts itself away in the same
breath. `zwp_input_method_v2.grab_keyboard` is the input method's own way in:
the compositor routes key events to the shell instead of to the application,
and the field the board is typing into does not move at all. The grab lasts
exactly as long as the board does.

### The layout

The arrangement is ANSI, and complete: the function row from `Esc` to `F12`,
the number row starting at the backtick, Tab, Caps Lock, Enter, both Shifts,
the backslash where `\` belongs, Ctrl and Alt where the bottom row keeps them,
and an arrow cluster beside the space bar. The key widths are ANSI's own — Tab
1.5 keys, Caps 1.75, Enter 2.25, right Shift 2.75 — so the stagger down the
sides is the one printed on the keyboard the user already knows, and every row
comes to exactly the same width.

Two departures. The function row is drawn at a little over half height, which
is what a keyboard that has one looks like and what stops thirteen keys nobody
visits while typing a password from taking a sixth of the board. And the bottom
row keeps only the two modifiers that can be reached without holding anything
down, giving the space the rest would have taken to the arrows and the way out.

Every modifier latches, because a board driven one key at a time has no way to
hold anything: there is only ever one finger on it. Shift, Ctrl and Alt arm for
a single key and lock when pressed twice, so one capital costs one press and a
run of them costs two, and `Ctrl` then `C` is a copy. Caps Lock goes straight
to the lock, which is what it is for. A latched key stays lit after the cursor
leaves it, brighter when locked than when armed. There is no second page of
symbols: Shift reaches them, in the places they are printed.

Shift is the one modifier not sent as a modifier. The board's keymap gives
every capital a key of its own, so a shifted letter is reached by sending `A`
rather than `a` with Shift held — which also means nothing can be left latched
in the application if the board goes away mid-word. Ctrl and Alt have no such
key to send, so they go as the mask they are.

Five caps are drawings rather than words: the four arrows, because the shell
bundles Roboto precisely so it does not depend on what fonts a console has
installed and Roboto has no arrow glyphs; and the way out, because a keyboard
folding away is recognised across a room where a word has to be read. They live
with the other built-ins in
[`crates/lxb-desktop/src/glyphs/`](../crates/lxb-desktop/src/glyphs).

The keyboard is drawn over the application without taking its keyboard, and
that is not a nicety — it is the condition of the thing working at all.
Keyboard focus is what carries text-input focus, so a board that took the keys
would take them from the field it exists to type into: the application's text
input would deactivate and the board would put itself away in the same breath
it appeared. It is therefore driven from the controller, and lets the pointer
through to the application underneath.

### How it knows

Three standard protocols, none of them LineXinBar's own:

- `zwp_text_input_v3` is the application saying a field has the cursor.
- `zwp_input_method_v2` is the shell hearing about it. Its `activate` event is
  the only signal Wayland has that a keyboard should come up — nothing else in
  the protocol describes what is *inside* a window.
- `zwp_virtual_keyboard_v1` is the shell typing: it uploads a keymap of its own
  and sends keycodes through the seat, so the letters reach a terminal, a game
  and an X11 client as well as the field that asked for them.

The honest limitation, and it is every Wayland keyboard's: an application that
never sends `zwp_text_input_v3` has no way to say a field is focused, so the
board cannot come up by itself there. That covers Chromium without
`--enable-wayland-ime`, and every X11 client. It can still be summoned, and
typing into it still works — which is why the shortcut is unconditional rather
than a convenience: the applications that most need it are exactly the ones the
shell cannot tell have a text field, so it does not try to guess.

Summoned over the bar with nothing running, the board opens and types nowhere,
because there is no application to receive the keycodes. That is deliberate: a
shortcut that silently refuses on some screens is harder to learn than one that
visibly does nothing.

### The corner hint

When a field has the cursor, the guide menu is closed and the keyboard is not
up — which is what you get after dismissing it — a chip appears in the bottom
right of the display, over the application, naming the chord that brings it
back. It goes when the cursor leaves the field. Unlike the board itself, the
hint needs an application: it points at a text field, and a text field belongs
to one.

It names the two buttons by drawing them rather than by lettering them.
"Press X" is wrong on a PlayStation pad, which has no X, and worse than wrong
on a Nintendo one, where X is the button *above* the one meant — the letters
are swapped between the two commonest layouts. "Press Select" is no better; the
same button has carried five different names. A picture of the pad with one
button filled in is true on all of them. Both drawings are in the tree, at
[`crates/lxb-desktop/src/glyphs/`](../crates/lxb-desktop/src/glyphs), and
compiled into the binary the way the font and the shaders are.

## Mouse and touch

The shell is drawn for a controller and none of that changes to be pointed at.
What a click does is what moving the selection there and pressing `A` does,
carried out through the same actions, so there is one answer to what every
control means rather than two that can drift apart.

One rule shapes the rest of it:

> Things that **stand still** light up under the pointer and answer the first
> click. Things that **move when they are selected** take one click to select
> and another to act.

The guide's sidebar, the context menu, the modal panel's buttons, the power
question and the on-screen keyboard are the first kind: hovering a row *is*
selecting it, exactly as it always has been on the board. The start screen's
own rows and the overview's cards are the second, and the reason is the shape
of the bar: selecting a row slides it to the middle of the screen, so a hover
that selected would pull whatever was under the cursor away and leave its
neighbour there to be selected in turn. The cursor would walk the bar across
the display with nobody touching the mouse.

| Input                    | What it does                                     |
| ------------------------ | ------------------------------------------------ |
| Left button              | Presses what is under it, or selects it first     |
| Right button             | Opens the context menu on what is under it        |
| Side button (rear)       | Opens the guide overlay, from inside anything     |
| Wheel                    | Moves the selection: the column, the rows of a menu, the deck of cards |
| Clicking on another display | Hands control to that display, and nothing more |

Control is taken, never wandered into. A pointer resting on the other screen
changes nothing there: it is answered by the shape of the cursor and by nothing
else, and the display holding control goes on holding it until a click, a
shoulder button or `Tab` says otherwise. Control used to follow the pointer
across, which made a mouse crossing a screen edge — on the way to somewhere
else — enough to move the guide, the keyboard and the next launch onto a
display the user was not using. The first click on a display without control is
spent taking it, for the same reason: what that click would otherwise press was
chosen while another screen was being driven, and the overlay it appears to
land on was somewhere else when the button went down. The wheel is the one
exception, because turning it *is* an act on the display under it: it takes
control and then turns what it has taken.

The quick-settings bars and the mixer's rows are the one place a press carries
something with it: on the groove it sets the value where it was clicked, and on
the speaker at the groove's head it silences — which is what pressing the bar
has always meant.

Touch is the same hit test without the hovering. A finger going down selects,
lifting it presses, and a finger that has travelled more than a couple of dozen
pixels has stopped being a tap and presses nothing however it ends.

The pointer only reaches what the shell is actually showing. The input region
is cut to the same answer the drawing is, so a keyboard drawn over a game takes
clicks on its keys and nowhere else, and a launch splash takes none at all.

## The cursor

There is no cursor on screen until something moves one. A console is driven
with a controller, and an arrow parked in the middle of a start screen is a
thing the user cannot move and did not ask for, so the session comes up without
one and a machine with no mouse plugged into it never grows one.

| What the user does | The cursor |
| ------------------ | ---------- |
| Moves a mouse or a touchpad | Appears, where the movement puts it |
| Aims with [the stick pointer](guide.md#the-stick-pointer) | The same: that stick *is* a mouse — inside the application it was turned on for, which is as far as it goes |
| Presses a key on a keyboard | Goes away |
| Presses anything on a controller | Goes away |

Movement is the only thing that brings it back, and that is the whole of the
rule. A cursor that reappeared because a button was pressed would appear
wherever it happened to have been left, which is nowhere the user is looking;
moving it is the one gesture that also says *where*.

The keyboard half is the compositor's own — it sees those keys. The controller
half cannot be, because a controller is not a seat device: the shell reads the
pad straight from `/dev/input`, which is the only reason the guide button works
while a game holds the keyboard, and it leaves nothing for the compositor to
notice. So the shell says so, with `hide_pointer`. It says it for the presses
it acts on itself and not for the ones the stick pointer is clicking with —
with the stick pointer on, the whole pad is the mouse, and a cursor that
blinked out at every trigger pull would be one the user could not use.

Typing on the on-screen keyboard does not hide it either, for the same reason
in reverse: those keys arrive as a virtual keyboard rather than as a seat one,
and the hand pressing them may well be on the mouse.

## Keyboard focus

The shell asks for `Exclusive` keyboard interactivity whenever nothing is in
front of it, so the launcher is immediately controllable, including when it is
nested. With an application running it drops to `OnDemand`, and the compositor
then resolves focus in this order:

1. a layer surface demanding `Exclusive`,
2. otherwise the topmost application window,
3. otherwise any layer surface willing to take input.

That last step is what returns focus to the shell when the application exits.
Controller actions are paused whenever keyboard focus leaves the shell, so the
bar cannot react behind a running game. Pass `--grab-keyboard` to retain the
exclusive grab and controller input at all times instead.

Applications launched by the shell are explicitly given the same named
Wayland socket as the shell. It accepts an X11 display only through LineXinBar's
`LXB_XWAYLAND_DISPLAY` marker; arbitrary host `WAYLAND_SOCKET` and
`DISPLAY` values are discarded. The nested helper also starts a private D-Bus
session, preventing ordinary D-Bus activation from forwarding a launch to an
outer-desktop process. Applications with their own profile-based remote IPC
can still reuse an existing instance while nested; test those with a separate
profile or from the intended dedicated LineXinBar login session, where no host
desktop instance is running.

The shell and private XWayland server are one session unit. If their XWM
connection is lost, LineXinBar exits instead of leaving the shell with a stale
`DISPLAY`; a production service manager can then restart the complete session.
Waiting for XWayland's display-ready signal has a five-second deadline and
falls back to a Wayland-only shell if the server is missing, broken, or never
becomes ready.

## Default keybindings

| Binding                | Action                          |
| ---------------------- | ------------------------------- |
| `Ctrl+Alt+Backspace`   | Quit the compositor             |
| `Super+Q`              | Close the focused window        |
| `Super` on its own, `Super+Home`, `XF86HomePage`, mouse side button | Show the guide overlay |
| `Super+K`, `XF86Keyboard` | Show the on-screen keyboard  |
| `Print` (with anything held), `Ctrl+Shift+3`, `Alt+Shift+3` | Photograph this display |
| `XF86AudioRaiseVolume` / `XF86AudioLowerVolume` / `XF86AudioMute` (with anything held) | Turn the session up or down, or silence it |
| `Super+Tab`            | Cycle windows on this output    |
| `Alt+Tab`, `Alt+Shift+Tab` | Walk the [guide's deck](guide.md#the-cards) while the modifier is held, and take what it lands on |
| `Super+←` / `Super+→`  | Focus the previous/next output  |
| `Super+Shift+→`        | Move the window to the next output |
| `Ctrl+Alt+F1`…`F12`    | Switch VT (udev backend only)   |

The Windows key is the home button, and it is watched for rather than looked up
in the table: a bare modifier is half of every chord in it, so a guide that
opened on the press would shadow all of them. What names the key on its own is
the release — down, up, and nothing in between — and both edges still reach the
client, because swallowing the release of a modifier whose press was forwarded
leaves an application holding a Super it is never told about again.

Any of these can be overridden in the `[keybindings]` table, except the guide:
its chords always summon the home menu, ahead of every other binding, and
`"<chord>" = "guide"` adds another one rather than moving it.

