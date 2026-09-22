# Desktop settings

[Documentation](index.md) · [Project home](../README.md)

- [Appearance](#appearance)
- [Display](#display)
- [Sounds](#sounds)
- [Network](#network)
- [System](#system)

## Appearance

### Accent color

`Settings > Appearance > Accent color` is the shell's own colour: **Purple**,
which is what it comes up in, plus **Blue**, **Green**, **Yellow**, and **Red**.
Each is drawn in itself, and the one in force carries a tick.

An accent is a whole palette rather than one colour. Previewing one smoothly
changes the selection glow, the lit rim of a chosen pane, the glass and the
bloom under the icon the cursor is on — and the wallpaper with them, since its
ribbons and aurora are drawn *in* the accent, and a sky that no longer belonged
to it would read as two themes fighting. Nothing is reloaded to make that
happen: every colour in the shell is read as it is drawn, so every display
travels through the same transition at once.

It is written to `~/.config/lxb/shell.toml`, which is a file you can also
just edit:

```toml
accent = "Blue"
```

An unknown name there is reported and ignored, and the shell comes up violet.

### Theme

`Settings > Appearance > Theme` is what the shell is *made of*, and it is a page
rather than a list: **Wallpaper** and **Icons**, each offering **Default** and
**Simple** — and the wallpaper one thing more, [a picture or a film of your
own](#custom-wallpaper).

Under `Default` the wallpaper's current is a band of water three sheets thick,
lit as bodies, and every one of the shell's own marks is a bead of water shaded
out of its own distance field. `Simple` stands that down — the current becomes
the three fine glass-silk ribbons the shell drew before the band, and a mark
becomes the flat shape of itself. The *drawings* never change, only what they
are made of, which is what keeps the shell recognisable rather than reduced.

The two are separate settings because they are separate expenses and separate
tastes. The wallpaper is one evaluation of a long function for every pixel of
every screen on every frame; a mark is a few dozen pixels of a row. On an
RX 9060 XT one full-screen evaluation is 0.38 ms at 1080p under `Default` and
0.20 ms under `Simple`, and a mark goes from six reads of its distance field to
one — so a machine that cannot pay for the water behind everything can very well
keep the beads in front of it, and somebody who simply prefers flat marks can
have those over the moving water.

Both rows preview: highlighting a value draws the shell in it without choosing
it, and walking back off puts the applied one back. There is no transition,
because there is no halfway between a bead of water and the flat shape of one —
and previewing is essential here, since neither value's *name* tells anybody
what they are looking at.

They are written to `~/.config/lxb/shell.toml`, which you can also just edit:

```toml
theme-wallpaper = "Default"
theme-icons = "Simple"
```

The login screen reads both keys and the compositor reads the wallpaper's, so a
machine set to `Simple` is in `Simple` from the moment the greeter appears and
never changes material in front of you. A file from before the setting was split
carries one `theme` key; it is still read, and both halves take it.

### Custom wallpaper

`Settings > Appearance > Theme > Wallpaper > Custom wallpaper` is the third
answer under Wallpaper, and it is not a material: it is a picture or a film of
your own, standing where the shell's scene would be. Choosing it stops the
background shader drawing a scene at all — no gradient, no lights, no aurora, no
band of water — and puts your file there instead.

The row opens the shell's own file browser, on the same three places Files opens
on: your home directory, the machine from `/`, and every drive that is mounted.
It lists **only what could stand behind a screen** — the folders to keep walking
through, pictures, and films — and pictures show themselves on their rows, as
they do everywhere else in the shell. There is no context menu in there: the file
is the answer to a question, not something to be copied, renamed or thrown away
from a column you opened to choose a wallpaper. Press one and the bar comes back
out to the Wallpaper column with Custom wallpaper ticked and the file's name
under it.

**It is the wallpaper, not a layer over it.** Glass panes refract it, the guide
blurs it behind the overlay, the overview draws it inside the start screen's
card, and a game's key art still lies over it exactly as it lies over the scene.
The picture is cropped to fill the display rather than stretched into its shape.

**A film plays silently, and cannot do otherwise.** Its audio stream is never
opened, no audio decoder is ever made, and the only thing in this shell that
makes a noise is its own embedded clips — see [Libraries](getting-started.md#libraries). It loops,
it is decoded at its own frame rate and no faster, and it stops dead when nothing
is drawing it: an application filling the screen, or a display that has gone to
rest, costs one sleeping thread.

The file you choose is **copied into `~/.local/share/lxb/wallpaper/`**, under
its own name, and it is that copy the shell reads from then on. Tidying your
Downloads folder, renaming the picture or unplugging the stick it came off does
not take your wallpaper with it. The copy happens on a thread — a film can be several gigabytes — while the
picture you chose is already on screen.

```toml
theme-wallpaper = "Custom wallpaper"
wallpaper-file = "/home/you/.local/share/lxb/wallpaper/sunset.jpg"
```

If that file is not there when the session starts — a drive not plugged in this
morning — the shell draws its own wallpaper for the session and **leaves the
setting alone**, so plugging the drive back in brings it back. A file that
nothing here can decode, pressed just now, puts the setting back to `Default` in
front of you.

The login screen and the compositor both draw the shell's own scene for this
setting rather than your picture, and neither is being lazy: the file is under
your home directory, and both of them run before your session does — the greeter
as its own user, in front of every account on the machine. So the frame that
bridges the start of the session is the water, in your accent, and your own
wallpaper appears with the shell.

### Battery percentage

`Settings > Appearance > Battery percentage` decides whether the start screen's
corner writes the charge out in figures over the mark that draws it. **Off**,
until somebody asks for it.

**The row is there only on a machine with a battery**, and so is the mark. What
counts as one is the kernel's own answer, read from `/sys/class/power_supply`:
a supply of type `Battery` whose `scope` is not `Device` and which is actually
in its bay. That last pair of conditions is doing real work — a desktop with a
wireless mouse on it lists a battery, and it is the mouse's; a laptop with the
battery taken out still lists the bay it came from. Neither is a machine this
corner has anything to say about, and on both it says nothing rather than
drawing an outline or greying a row out. No daemon is involved: UPower is what
a desktop environment would ask, it may not be installed, and what it reads is
this same directory.

The mark itself is not the setting. It is one of six drawings of the same
shell — empty, low, half, high, full, and a bolt for a battery that is filling
— and it is drawn whenever there is a battery, in the same water as the clock
beside it. It appears twice: in the start screen's corner, and in the
[guide's header](guide.md#the-guide-overlay) under the day. One size in both, because what
decides that size is the material rather than either layout — the shell's wall
is 2.4 units of the drawing, and what matters is how many pixels that lands on
at 1280x800. Being on the mains outranks the level: while it is filling, the bolt
is what is shown, and the level it is filling from is what the figures are for.
Sitting plugged in at full is not filling, and shows a full battery.

The figures are off by default because the mark already answers the question a
glance at a corner asks, which is *how much is left*. A number is for somebody
who wants to know whether it is 61 or 68, and a console that put one on the
wallpaper for everybody would be asking everybody to read it.

```toml
battery-percent = true
```

The key is written on every machine, a desktop included, so that a file carried
between one and a laptop does not lose the setting on the way.

Entries are read directly rather than through the XDG menu files, so the
`.menu` layouts a desktop ships (Plasma's vendor submenus, its Lost & Found, and
anything kmenuedit has rearranged) do not carry over. `OnlyShowIn` and
`NotShowIn` are honoured against `XDG_CURRENT_DESKTOP`, which the session sets
to `Lxb`: an entry written for one specific other desktop stays in that
desktop's menu.

## Display

`Settings > Display` is the one part of the Settings column the shell does not
carry out itself. It sends what was chosen over `lxb_shell_v1` and the
compositor does the work, because none of it is a client's to touch. There are
seven pages: **Resolution**, **Refresh rate**, **Orientation**, **Application
scaling**, **Night light**, **HDR**, and **OLED protection**.

Every one of them is *per screen*, and every one of them names the screen
before it offers anything — see below, where the rule is written out once for
HDR and holds for all six.

### Resolution and refresh rate

```
Settings > Display > Resolution    >  DP-1  >  2560 × 1440
Settings > Display > Refresh rate  >  DP-1  >  144 Hz
```

Two rows rather than one list of modes, because they are two questions. A
television reports the same six sizes at four rates each, and a single list
would be two dozen rows most of which repeat the size above them. So the sizes
are listed once each, saying what the best rate at that size is, and the rates
are asked for separately.

**Every size the connector lists is on the first page.** The second page is a
page about the size that screen is set to, and lists **every rate that size
carries** — no more, because a refresh rate is not something a display has on
its own. It is something a *mode* has: a monitor that does 240 Hz at 1440p and
120 Hz at 1080p does not do 240 Hz at 1080p, and a page offering it there would
be offering a mode the connector has never listed. Pick the size first and the
rate page follows it.

Which means only one of the two moves the other:

- A size takes the current rate with it where the new size carries it, and asks
  for the fastest of that size where it does not.
- A rate never moves the size. It sets the rate, at the size it was listed
  under.

Neither ever asks for a mode the connector has not listed, and every row does
what its title says.

Two of a display's modes can round to the same readable rate — 119.998 and
120.000 are both "120 Hz", and high-refresh panels list both. Neither is
dropped: where the short form would collide, every rate in the collision is
printed to the thousandth of a hertz it is counted in.

Every screen the compositor reports modes for is listed on both pages, even one
with a single mode: that row is what says what that screen is showing. On the
rate page it names the size as well (*120 Hz at 3840 × 2160*), because the
rates underneath it are that size's. A nested session reports no modes at all,
because the size of its window belongs to the compositor LineXinBar is running
inside; where nothing reports any, the row says so rather than opening onto an
empty column.

The mark is on what the display is **actually** running, not on what was last
asked for: the answer comes back over the same protocol in the same breath as
the change, and a mode the hardware refused must not read as chosen.

### Orientation

```
Settings > Display > Orientation  >  DP-1  >  90° Rotation
```

For a screen standing on its side. Four turns, on every screen the compositor
turns itself:

| | |
| --- | --- |
| **0° Rotation** | Landscape, the way the display is built. |
| **90° Rotation** | Portrait, for a screen turned clockwise. |
| **180° Rotation** | Landscape, for a screen hung upside down. |
| **270° Rotation** | Portrait, for a screen turned the other way. |

Each row is a *drawing* of the monitor stood that way, its stand saying which
way up — the same monitor the screen list beside it is drawn with. They are the
only values in the Settings tree with pictures of their own: a brightness in
cd/m² has no shape, and inventing one would be drawing a picture of a number,
but an orientation is a shape, and the row matching the screen in front of you
can then be picked without reading it. The degrees are what the drawing cannot
say — which of the two portraits this is, and how far from where the display
started.

Unlike a mode, none of these is a list the display has: nothing here reaches
the connector. The picture is composited turned and scanned out at the mode's
own pixels, which is why no hardware can refuse a turn and why every screen is
offered all four. What a quarter turn does move is everything else — the
display's logical width and height swap, so the screens laid out beside it
shift along, the bar and every other layer surface are re-arranged to the new
shape, and every window on it is re-tiled. That is the whole reason it is the
compositor's to do.

**90°** is the turn a screen swivelled clockwise wants — the picture is drawn a
quarter turn anticlockwise, which stands up on a monitor whose foot has gone to
the left — and **270°** is the other one. The four mirrored orientations
`wl_output` also has are not offered: a mirrored picture is a projector rig
rather than a way up. The compositor's own config can still set one, and a
screen in one is named in the row above the list with none of the four marked,
because it is in none of them.

Which screens are listed is the compositor's answer rather than a guess: it
reports an orientation for every display it turns itself, and for no others. A
nested session is the ordinary "no others" — the way up of its window belongs
to the compositor LineXinBar is running inside — and where nothing reports one,
the row says so instead of opening onto an empty column.

### Application scaling

```
Settings > Display > Application scaling  >  DP-1  >  150%
```

How large the applications on one screen draw their own interfaces. It is a bar,
not a list, and the same object the night light's colour temperature is set on:
every five per cent between 100% and 300% is a sensible answer, and as rows that
would be forty-one of them standing for a quantity that has no steps in it. Up
and Down move the value, Left leaves, and a click along the groove goes straight
to the size it landed on. The bar itself reads `150%` over `Half again as large`,
so the usual question is answered without doing arithmetic.

**100% is the floor.** The bar starts there — one to one, every application at
the size it chose — and there is no step below it: an application asked to draw
its interface *smaller* than it chose is a thing to want at a desk two feet from
a 4K panel, and this shell is driven from an armchair. A number below 100 in the
file, or from an older shell, is read as 100.

It is not a magnification. What the compositor does with it is give each
application a logical window that much smaller than the display it is on and tell
it — over `wp_fractional_scale_v1` — that its scale is that much higher, so the
client renders a buffer with exactly as many pixels as the screen has and those
pixels are put on it one for one. At 200% on a 1280×800 display a window is
configured at 640×400, hands over a 1280×800 buffer, and its text comes out twice
the size and just as sharp — the same thing a high-density laptop panel does to
every toolkit on it. A client that ignores the scale is drawn at the size it
chose and enlarged, which is soft, and is the answer such a client gets
everywhere.

**Per screen, and it did not use to be.** This was one number for the session,
under `Settings > System`, on the argument that how large an interface has to be
to be read is a fact about the person in front of the screens rather than about
one of them. That is true and it is not the whole of it: a person at a desk with
a television behind them is sitting two distances at once, and one answer for
both had to be wrong about one of them. A window carries the answer of the screen
it is on, so moving one between screens configures it again for the screen it
lands on.

**The shell is not affected.** It draws itself in layer surfaces sized against
the display it was given, so the bar, the guide and this very page stay exactly
where they are at any setting — which is the whole reason this is done per window
instead of by moving the output's own scale.

**Neither is anything under Xwayland.** X11 has no per-surface scale to tell a
client about, so the only thing that could be done to those windows is to
magnify pixels they have already drawn, and a blurred window is not what
somebody asking for a larger one asked for.

It is written to `~/.config/lxb/shell.toml`, under the screen it was set on:

```toml
[display.DP-1]
application-scale = 150
```

A file written before the row moved has `application-scale` at the top level
instead, and that still means what it meant: every screen at that size, including
one plugged in later. It is read as the value each screen inherits, and it is
written back unchanged.

The compositor remembers nothing about it, which is the one place this differs
from a mode or a night light. Those are written down by the compositor because it
lights the displays a second before the shell can speak and being corrected
afterwards costs a black screen; here there is nothing on screen to correct —
every application is started *by* the shell, always after it has said what this
is.

### Night light

```
Settings > Display > Night light  >  DP-1  >  Schedule  >  Sunset to sunrise
```

The blue light filter: everything on a screen tinted towards a warmer colour
temperature, so a display looked at in the evening is not a daylight-white one.
It is the CRTC's gamma ramp — green and blue scaled down against red — which is
why it is the compositor's to carry out, and why it composes with HDR instead of
fighting it: both are encoded into the same ramp, in one commit, so turning one
on cannot undo the other.

Three rows on every screen that has a ramp, and five where the schedule is one
with hours in it:

| | |
| --- | --- |
| **Night light** | Off, or on. Off is the whole of off, whatever the schedule says. |
| **Color temperature** | A bar, 2000 K to 6500 K. Lower is warmer. |
| **Schedule** | **All day**, **Sunset to sunrise**, or **Custom hours**. |
| **From** | The hour of local time it comes on at. *Custom hours only.* |
| **Until** | The hour it goes off at. *Custom hours only.* |

Turned on for the first time it is an **evening**: 22:00 to 06:00, at 4000 K.
That is the one default here that can look like nothing happening — switched on
in daylight it warms nothing until ten — which is why the row's own comment
reads *On at 22:00* rather than *On*. The alternative is worse: a filter whose
whole purpose is the evening, coming on the moment it is asked for at eleven in
the morning, is a filter most people would turn straight back off.

#### The temperature is a bar

`Color temperature` is the one setting in the tree that opens onto a **bar**
rather than a list, because it is the one whose answers are a *scale*. Every
hundred kelvin between candlelight and daylight is a sensible thing to want; as
rows that is forty-five of them, which is a column nobody can scan standing for
a quantity that has no steps in it to begin with. The short list it replaces was
eight arbitrary points, and somebody who wanted the one between two of them
could not have it.

Up and Down move the value, which is what those two mean in every other column —
there is simply nowhere for a cursor to go, because the bar is the whole of its
column. **Left still leaves**, exactly as it leaves any other column, so nothing
new has to be learnt either to use it or to get back out of it. The bar stops at
both ends rather than wrapping, and a held direction crosses the whole range in
a moment.

The filled part is drawn **in the colour of the light it stands for**, which is
the one thing neither the number nor the words can be: a picture of what the
screen is about to look like. Higher up the track is more kelvin — cooler, less
filter — so a full white bar reads as what it is, no warming at all, and a short
orange one as candlelight.

The groove, the light lying in it and the handle are each a slab of the **same
glass every other control in the shell is cut from** — bevelled rim, the sheen
down that bevel and the colour split at its edge all worked out from the one
lamp the shell is lit by, not painted on. The groove *is* the row's glass: it
stands where the disc under a chosen icon would have stood, and there is no disc
under it, because a round pane behind a tall track is a button that has been
pressed with a bar lying across it.

The number is still said in kelvin rather than as a percentage of some
undeclared maximum: 2700 K is the bulb in the lamp beside the screen, and
somebody who knows that knows what the bar will do before they move it. The line
under it carries the strength in words for everybody else — *A filament bulb*,
*Distinctly warm, like a lamp*. The warm end stops at 2000 K because below
roughly 1900 K a black body has no blue in it at all, and a screen with its blue
channel taken to zero does not show blue-on-white text as warm, it shows it as
blank. 6500 K is daylight and is exactly the picture with the filter off — the
ramp is normalised so that it is the identity to the last code, rather than
nearly one.

#### The schedule

**It is the shell's, not the compositor's.** A schedule is a clock and a time
zone, and a compositor has no business owning either; so the shell works out
whether the light should be burning at this moment and sends only the answer.
Nine in the evening arriving is then an ordinary change to a value the shell was
already comparing every pass of its loop, carried out by exactly the path a
button press goes through.

**Sunset to sunrise** needs no hours at all: both ends move every day, and the
row says which they are today and where they were worked out for — *20:10 to
05:13 today, at Europe/Warsaw*. The location comes from the **time zone's own
coordinates**, out of the zone table the system's time zone data already ships.
That is deliberately the cheapest of the honest answers: a geolocation daemon
may not be installed and asks a permission question a colour setting has no
business raising, an address looked up over the network is the user's location
leaving the machine, and asking them to type a latitude is asking them to go and
find one. This is already on the disk, already theirs, and costs one file read
for the whole session.

What it gives is the zone's representative city rather than a position, which
inside a large zone can be a few hundred kilometres out — some tens of minutes
of sunset, and far inside what a night light cares about. Somebody a long way
from that city can write `night-light-latitude` and `night-light-longitude` into
`~/.config/lxb/shell.toml`; there is no page for them, because a page asking for
a latitude would be asking the user to look one up. Where the machine names no
place at all the row is replaced by the reason there is none, rather than
offering a schedule that could never come on. Above the arctic circle the two
honest readings are kept: on a day the sun does not rise the light burns through
it, and on one it does not set it does not come on at all.

**Custom hours** is the two rows below. They wrap past midnight, which is the
ordinary case rather than the exception — *21:00 to 07:00* is an evening, and
each ending hour says how long the window it makes lasts. The end is exclusive:
a light that goes off at 07:00 is off at seven. The two ends may never be the
same hour, so the **Until** page lists twenty-three of them and leaves out the
one the window starts on: a window that ends where it begins is neither a whole
day nor none of one, and there would be no way to look at that row and tell
which it meant.

Changing the schedule puts the hours aside without forgetting them — asking for
Custom hours again gives back the evening that was there, not one the shell made
up — and while they are not being kept, **the two rows are not on the page at
all**. Hidden rather than explained, because they are not the schedule's detail
so much as *one* schedule's: a page that is following the sun and still shows
*From 22:00* is making a claim about tonight that is not true, and no wording
inside the row undoes two hours sitting there in plain sight.

Which screens are listed is the compositor's answer again, and it is a much
longer list than HDR's: warming a picture needs a gamma ramp and nothing else —
no EDID claim, nothing of the link — so an ordinary SDR laptop panel that will
never do HDR is on this page. What drops off is a nested session, which owns no
CRTC and therefore no ramp; where nothing can be warmed, the row says so rather
than opening onto a column of controls that would all be inert.

### HDR

`Settings > Display > HDR` turns high dynamic range on. Both halves of it are a
modesetting client's: the connector has to be told the signal is BT.2020 with
the ST 2084 (PQ) transfer function, and the CRTC's colour pipeline —
`DEGAMMA_LUT` → `CTM` → `GAMMA_LUT` — has to re-encode the composited sRGB
picture into it. Without the second half, a display told to read sRGB numbers
as PQ shows a scene far too dark and the wrong colour.

On a desk with more than one HDR screen, it names the screen first:

```
Settings > Display > HDR  >  DP-1  >  SDR brightness  >  250 cd/m²
```

Every screen that can be driven in HDR is listed under it, by the connector
name the compositor logs at startup, with what it is doing and the peak it
reports. Screens appear when they are plugged in and disappear when they are
not; nothing has to be configured for a new one to show up, and nothing in the
shell knows the name of any particular display.

The screen comes before the settings because the settings are per screen, all
the way down to the file they are written to. An HDR television beside an SDR
laptop panel is the ordinary case, and one set of answers for both would be
describing a machine nobody has. Unplugging a display does not forget what it
was set to; it comes back the way it was left, because both halves file the
settings under the connector.

With **one** HDR screen there is nothing to choose between, so that step is not
there:

```
Settings > Display > HDR  >  SDR brightness  >  250 cd/m²
```

A question with a single answer is not a question, and a level that exists only
to be walked through reads as though something else were on offer. The screen
is named in the HDR row's own subtitle instead, so it is still clear what the
settings belong to. Screens that cannot do HDR are what decides this as much as
screens that can: one capable screen beside three SDR ones still collapses,
because the list would have had one row. A session where *none* can shows a
single row saying so — a subcategory you cannot step into is worse than one
that explains itself.

| Setting | |
| --- | --- |
| **HDR** | Off, or on. Refused, and reported as such, unless the display's EDID advertises PQ and the driver has a colour pipeline to feed it. |
| **SDR brightness** | What plain white is sent at, 80 to 400 cd/m². |
| **sRGB color intensity** | 0% to 100%: how far sRGB's colours are stretched into BT.2020's much wider gamut. |
| **Peak brightness** | What the display is told to expect, or *Display default* — the peak it reports in its EDID. |

**SDR brightness** is the control that matters most, because nothing LineXinBar
composites is HDR content: every application and the shell itself are SDR, so
this alone decides whether the session comes out dim or blinding. sRGB's own
reference is 80 cd/m², which describes a darkened grading suite; the default is
200, which is roughly an ordinary lit room.

**sRGB color intensity** is the choice between the two honest things to do with
an sRGB picture inside BT.2020. At **0%** its colours are placed where they
actually belong, so the session looks exactly as it did in SDR. At **100%** the
numbers are sent through untouched, so sRGB's red is displayed as BT.2020's red
and everything comes out far more saturated — the "vivid" mode a television
ships in. In between is a blend of the two, and because both ends are matrices
whose rows sum to one, so is everything between them: white stays white at
every setting.

It is the one control here that some hardware cannot honour, and the one that
says so. It is the `CTM`, and a matrix is only a gamut conversion when it acts
on linear light — so it needs a `DEGAMMA_LUT` in front of it, which not every
display engine exposes (amdgpu on RDNA 4 does not, on its display pipes). Where
there is none, the sRGB decode is folded into the gamma curve so brightness
stays right, the matrix is left at identity, and the gamut sits at the vivid
end. On such a screen this row offers no choice at all: it reads *Not available
on this display*, and opening it gives the reason. The compositor tells the
shell which displays can do it, per display, so the answer is right on a
machine where one screen can and another cannot.

Everything on the page is applied as one atomic commit, which the driver is
asked to validate before it is made for real — a pipeline the hardware will not
take is refused by a commit that changed nothing, and the page reports the
display as still in SDR rather than showing the switch as having taken. Leaving
the session puts every display back into SDR first, so quitting always returns
a readable console.

Turning HDR **off** is a signal in its own right, not the absence of one. The
display is sent the same metadata infoframe carrying the traditional-gamma
EOTF, which is how CTA-861.3 has a source say the picture is ordinary again; a
sink that merely stops hearing about HDR has been told nothing, and one that
goes on decoding PQ shows an SDR picture as very little at all. Because those
properties can force a modeset, the compositor also re-reads the CRTC
afterwards and draws a full frame, rather than trusting that what it last put
on screen survived the driver rebuilding the pipe.

Each screen says what it is doing — *Ready* or *In HDR*, and the peak it
reports — in its own row where there is a list, and in the HDR row's subtitle
where there is not. Either way the state of the session is answerable without
stepping into anything. It carries no
value, so choosing it does nothing — a row that describes something true must
not be one the user can un-choose. A switch that does nothing has to have its
reason on the same page, not in a log.

Unlike the accent, none of this previews as the cursor passes over it.
Reconfiguring a connector cuts most displays to black for a second while the
panel resynchronises, and a setting that blacked the screen out every time the
cursor moved down the list would be unusable. Walking the list is an invitation
to read the names; the value is committed when it is chosen.

All of it is written to `~/.config/lxb/shell.toml` beside the accent, one
section per connector:

```toml
accent = "Blue"

# What a display with no section of its own gets, including one plugged in
# for the first time. The mode has no such default: it names something one
# connector offers, and the display beside it may not offer it at all.
hdr = false
hdr-sdr-brightness = 200

[display.DP-1]
mode = "2560x1440@144"
transform = "90"
hdr = true
hdr-sdr-brightness = 250
hdr-srgb-intensity = 50
hdr-peak-brightness = 0
```

`mode` holds both halves of the page, spelled and named as the compositor's own
config spells and names a mode, so a line can be moved between the two files
and mean the same thing. Without an `@rate` it asks for the fastest mode of
that size. `transform` is the Orientation page, spelled the same way and for
the same reason: `normal`, `90`, `180`, `270`, or one of the four mirrored
spellings the page does not offer. A display with no `mode` or no `transform`
line is left at whatever the compositor brought it up at.

The compositor's own `config.toml` has the same mode, the same transform and
the same four HDR settings per output, for a session with no shell — see
[docs/configuration.md](configuration.md).

### OLED protection

```
Settings > Display > OLED protection  >  DP-1  >  On
```

One switch, and what it does is rest this screen behind black while another one
is being used. An OLED panel keeps what it is shown, and the start screen is
the worst thing there is to keep: the bar sits in the same row of pixels every
second it is up, the clock in the same corner, and a second display left on it
through an evening's play is a display with a bar burnt into it.

The black is the compositor's own sheet over the whole screen — the cursor and
anything running on it included — because the shell owns one surface per
display and nothing else on it. It takes nothing away: the session goes on
taking input the whole time it is down, which is what makes moving the pointer
onto the screen the way to get it back. A second going down, a quarter of a
second coming back, and it reverses from wherever it has got to.

Three things stop a screen being rested, and none of them is a setting:

| | |
| --- | --- |
| **Nothing is open at all** | Something has to be in front of one of the displays — any application, whatever started it: a game, a film, a browser, an emulator, Valve's own storefront. The compositor answers it from the window in front rather than from what that window calls itself, because an application reaches the screen as `steam_app_…`, as its own name, or as nothing at all. When the last one closes, every screen comes back. |
| **This screen is being driven** | Control is on it, so the user is on it. |
| **Something on it is still painting** | A film on the second screen is exactly what a second screen is for. A film somebody *paused* is deliberately not spared — a paused film is a still picture, which is the thing this exists for. |

What is deliberately *not* on that list is what happens to be open on the screen
being rested. A paused game, a window nobody has touched, the start screen: all
of them are still pictures, and the two clauses above already spare every screen
somebody is actually at. A session with something open on each display would
otherwise be one where no screen could ever rest.

Past those, a screen rests five seconds after the user last did anything on it:
moved the pointer over it, took it over, or pressed something on it. Going back
to what was open starts that five seconds again, so switching between screens
never blacks one out in the middle of it.

It is written down per connector, beside the mode and the night light:

```toml
[display.DP-1]
oled-protection = true
```

Nothing of it reaches the compositor's own `config.toml`. Where the mode and
the night light are remembered there — a session with no shell still has to
come up in the right mode — this is a rule about what the *shell* is drawing
and who is looking at it, and a compositor with no shell has neither.

## Sounds

`Settings > Sounds` holds two kinds of thing: where the *machine's* sound goes
and comes from, and what the shell itself plays. The devices come first, because
a session is set up before it is decorated and because nothing else on the page
can be heard until the sound is coming out of the right place.

```
Settings > Sounds > Output device  >  the cards this machine can play through
                    Input device   >  the ones it can record from
                    Start music    >  Off
```

**Output device** and **Input device** set what the whole machine plays through
and records from — every application on it, not just this shell, and whether or
not LineXinBar is running when they start. Each row lists what the sound server
offers, with the card on the line the eye lands on and how it is being driven
under it, and marks the one in use; the row above the list names that one too, so
the usual question is answered without stepping in at all. Monitors are left out
of the inputs: every output has one — its own sound, offered back for recording —
and none of them is a microphone.

This is the one setting in the column the shell does **not** write down. The
sound server is what remembers a default device, exactly as it remembers the
volume, and a shell that kept its own copy would be a second opinion about it at
every login: a device chosen in any other mixer would be quietly undone the next
time this one started. So the choice is handed to the server — `pactl
set-default-sink` and `set-default-source`, which is what PipeWire and PulseAudio
both answer — and the mark on the row is read back from it.

Choosing does not preview. Moving every sound on the machine to a device as the
cursor passes over it would put somebody's film in the wrong room four times on
the way down a list of four; the names are what the list is for, and the sound
follows when a row is chosen.

A machine with no sound server has nothing to choose here, and the page says so
rather than opening onto nothing: without PipeWire or PulseAudio nothing decides
this for the machine, and each program opens the sound card itself. A server that
answers and lists no output is a different fact — a machine with no sound card —
and reads differently.

**Start music** is the start screen's [background
music](architecture.md#shell-audio) — on, which is what the shell comes up doing, or off. It is
the one recording the shell can be told not to play, because it is the one it
plays at somebody who has pressed nothing: every other sound is an answer to a
control, and a shell with a button that answered silently would be a shell with a
dead button on it.

Turning it off is heard at once rather than faded out. A fade is what an
application taking the display gets, because that is a handover; this is somebody
saying *stop*, and most of a second of music going anyway is not what they asked
for. Turning it back on starts the track from its beginning, exactly as
returning to the start screen from an application does.

It says nothing about how loud the rest of the shell is. Every click, the
keyboard and the shutter stay exactly where the mixer's `System` row left them —
that row is how loud the shell is, and this is whether one of the things it plays
exists at all. Silencing `System` still silences the music too, and turning the
music back on does not unmute anything.

Nothing about it previews as the cursor passes over it, for a reason the Display
pages do not have. Turning music off is instant and costs nothing; turning it
back *on* rebuilds the track from sample zero, so a cursor walked between the two
rows would answer with the same four hundred milliseconds of fade-in over and
over, which is not what the setting sounds like. It is written to
`~/.config/lxb/shell.toml` beside the accent:

```toml
start-music = false
```

## Network

`Settings > Network` is the third of the three pages about what a session does
with the world outside itself — the picture goes out, the sound goes out, and
this goes both ways. It is after the other two because it is the one of the
three a console can be used without.

```
Settings > Network > Wired  >  Connection             >  On
                               IP address             >  Automatic / Manual
                               DNS                    >  Automatic / Manual
                               Connection information >  IP address, router, …
                     Wi-Fi  >  Wi-Fi                  >  On
                               Networks               >  the air, strongest first
                                   Upstairs        ✓  >  IP address, DNS,
                                                         Disconnect, Forget
                                   The Cafe           >  Connect, Forget
                                   Next Door             (joins on the press)
                               Connection information >  IP address, router, …
```

The Networks column is the air and nothing else. It used to open with a **Not
connected** row, which was the answer to *which network is this radio on* for a
machine on none — and for a while the only way off a network short of turning
the whole radio off. That reason is gone: the way off a network is inside the
network now, where somebody looking for it looks. What was left was a row above
the list doing the same thing at a distance, one row of ceremony on every visit
to the page for a press most people make once. On a machine that is on none of
them nothing here is marked at all, which is the honest picture — the question
has no answer yet rather than an answer called none — and the column opens on
the strongest network in the air, which is what somebody who came here to get
connected was reaching for. A radio that hears nothing gets a line saying so
rather than a row that opens onto nothing.

**What is on the page depends on what is in the machine.** A desktop with no
wireless card has no Wi-Fi page at all; a machine with no socket has no Wired
page; one with neither has a single row saying so. That is the same rule the HDR
page is built by, and it matters more here: a Wi-Fi page on a machine with no
radio in it is not merely useless, it is a page that has somebody turning a
switch on and off looking for networks that were never going to appear.

The row at the head of it says what the machine is on — `Connected by cable —
1000 Mb/s`, `Connected to Upstairs`, `Not connected` — so the question people
actually come to this page with is answered without stepping into it. The cable
wins where both are up, because that is the one the machine is really sending
over.

**Wired** is a switch and the facts behind it. A socket with nothing plugged into
it is not offered the switch: there is nothing for On to do, and a row that took
the press and left the mark on Off would be the page arguing with something the
user can see from where they are sitting. Bringing a socket up with no profile
saved for it asks NetworkManager for the obvious thing — a wired connection that
takes its address from the network — rather than putting a form in front of
somebody who plugged a cable in.

**Wi-Fi** is the radio switch, then the air. The switch is one switch for the
whole machine however many cards are in it, because that is what NetworkManager
has; a radio switched off by a key on the machine reports as much, and the page
says so instead of offering an On that cannot happen. **Networks** lists what is
in range, the one in force first and the rest strongest first — by the five bars
a signal is drawn with rather than by the per cent, so a list somebody is
standing in reorders only when something really moved and not every time the
radio breathes — one row per name: a house with a repeater in it publishes the
same network from two radios, and they are one thing to join. Each row says what joining it takes and how well it
is heard: `Saved — 62% at 5 GHz` for one there is already a profile for, `WPA2 —
55% at 5 GHz` for one that will ask for a password, `Open — 91% at 2.4 GHz` for
one that will not.

**The network the radio is on is stepped into rather than pressed.** It is the
one row in that list whose press has nothing to do — joining a network the radio
is already on is a no-op — and it is the only row that has anything more to say,
because a wireless profile belongs to a *network*: a laptop keeps a fixed address
at the office and takes whatever it is given at home, and those are two profiles.
So that row carries the tick every value in force carries, and behind it are its
own **IP address** and **DNS** pages, then **Disconnect** and **Forget**.

That is also why the addressing is not on the Wi-Fi page itself. A card that has
been on four networks this week has four answers to *what address does this
take*, and a row up there would silently be about one of them.

**A network the machine remembers is stepped into too.** It has two things to do
rather than one — join it again, or forget it — and forgetting has nowhere else
in the shell it could live: the list is otherwise a list of what is in the *air*,
where what the machine remembers shows only as the word `Saved` in a comment. So
a remembered network opens on **Connect**, with **Forget** under it. That costs a
press, and it is the one thing on this page that takes something away; what it
buys is that somebody who typed the wrong password once has a way to make the
shell ask again. A network the machine has *never* been on keeps its single
press: there is nothing saved to remove and nothing to configure until it has
been joined once.

**Forget removes the network from the machine, not from the shell.** What goes is
NetworkManager's profile — the key that got the machine on, and any address
pinned on that network — so it goes for every program on the machine, which is
the only version of forgetting that is not a second opinion. There is no panel
between the press and the act, so the row itself carries what it costs: *Remove
this network: the password will be asked for again*. It wears the same waste bin
the context menu's Uninstall row wears, because it is the same act on a different
object.

Two things about where those rows stand. **Disconnect and Forget come after the
addressing**, because a column opens on its first row and the first row has to be
one that only opens another column — somebody stepping into their own network and
pressing Accept twice out of habit lands on `IP address`. And **a cursor the
shell moves never comes to rest on a row that acts**: pressing Disconnect turns a
four-row page into the two-row one a network the radio is off gets, and a cursor
that simply clamped to the end of what was left would put a thumb on Forget with
no press of its own in between.

**Disconnect is built for every joined network**, whether or not a profile can
yet be read for the device. It is the only way off a network in the shell, and it
must not depend on NetworkManager having got round to publishing an active
connection for a radio that is already associated.

**A password is asked for by the worker, not by the press.** Pressing a network —
its own row where nothing is saved for it, **Connect** where something is — only
ever means *join this*. Whether that needs anything typed depends on
whether NetworkManager already has a profile and whether that profile still
works, which is something it knows and the shell does not — so the press goes
down, and the panel comes back up if one is wanted, with the on-screen keyboard
under it. The same panel answers the case people actually hit: a network whose
password was changed on the router is one NetworkManager has a profile for and
cannot use, and a second or two after the press it comes back asking, with *That
password was not accepted*. Typing a new one corrects the saved profile in place
rather than replacing it, so a fixed address or a name server set on that network
somewhere else survives.

An **enterprise** network — 802.1X, the kind a university or an office runs — is
listed and cannot be pressed. It needs a user name, a certificate and a server's
agreement rather than a password, and this shell will not collect the first thing
and pretend. It is listed at all because leaving it out answers *my network is
not here* with silence.

Nothing about the page previews. Highlighting a network would take the machine
off what it is on and put it on whatever the cursor was passing — mid-download,
mid-call — and on a secured one it would raise a password panel for a row nobody
chose.

### A static address, and where the name servers come from

Both halves get the same two pages, because a fixed address is not a thing that
is true of a cable and false of a radio. They hang in different places, and that
is not an inconsistency: a socket has one profile, so its addressing is a
property of the socket as far as anybody using it is concerned, while a radio has
one profile per network.

```
Settings > Network > Wired            > IP address  >  Automatic
                                                       Manual         ✓
                                                       Address           192.168.1.50/24
                                                       Router            192.168.1.1
                                        DNS         >  Automatic
                                                       Manual         ✓
                                                       DNS servers       9.9.9.9, 1.1.1.1

                     Wi-Fi > Networks > Upstairs ✓  >  IP address
                                                       DNS
                                                       Disconnect
                                                       Forget
```

It is called **DNS** on screen because that is what every router's own page calls
it and what anybody looking for the setting will look for. The prose here goes on
saying *name servers*, which is what the three letters stand for.

The values Manual needs are *inside* its own column rather than beside it. They
could have been rows of the page above — the night light's hours are — but that
page would then carry five rows about addressing where it now carries two, and
two of the five would be a row called `Address` standing under one called `IP
address`. Inside, each column reads as what it is: here are the two answers, and
here is what the second one is set to.

**Manual pins what the machine already has.** NetworkManager refuses a manual
profile with no address in it, so switching to Manual has to supply one, and the
only address that is not an invention is the one the interface was given — which
is also what somebody means by the press: *keep this*. A socket that has never
been up has nothing to pin, and the row says so instead of being a press that is
accepted and silently fails. Going back to Automatic changes nothing but the
method: the pinned address stays in the profile, so turning DHCP on to see
whether it works does not throw away the static settings.

**Every field says whose value it is.** The panel a typed row opens covers the
trail that would otherwise have said — so the heading names the connection as
well as the value: `Address — Upstairs`, `DNS servers — Wired connection 1`.
`Address` on its own is the same panel whether somebody walked in through the
socket on the back of the machine or through the network the radio is on, and
those are two different profiles with two different addresses.

**Name servers are only offered a choice where there is one.** Under automatic
addressing, `Automatic` takes what DHCP hands over and `Manual` uses only the
ones named below it. Under a pinned address there is no DHCP running, so there
is nothing for them to be automatic *from* — the page says that and offers the
list, rather than an `Automatic` that would quietly mean *none at all*.

**The three values are typed, and that is a third kind of row.** A colour
temperature is a scale, which is what a bar is for; an IP address is neither a
scale nor a set — it is four numbers and a prefix, of which every one is as
likely as any other, and no list anybody could write would have the user's on
it. So pressing one opens a panel with a field and the on-screen keyboard under
it, holding what the value already is: changing the last number of an address
should not mean typing the other three again.

What is typed is **checked before it is handed over**, while the panel is still
on screen and can still be corrected. `192.168.1.50` with no `/24` after it is
the thing everybody types and is not an address a profile can use — nothing in
it says how much of the network is local — so it is answered with *add the size
of the network*, in the same line, in the same place, with what was typed still
in the field. Clearing a field is always allowed and always means something:
clearing the address hands the connection back to DHCP, and clearing the name
servers hands the question back to the network.

Setting one **does not take the link down**. The profile is written and the
interface is brought into line with it through NetworkManager's own `Reapply`,
which is the call that exists for exactly this. Where that is refused the
profile has still been written and comes into force the next time the interface
comes up.

**IPv4 only**, and that is a stated limit rather than an oversight: what people
mean by "give this machine a static address" is an IPv4 address, and IPv6
addressing is a form with a great deal more in it. IPv6 is left exactly as
NetworkManager had it, so a machine given a static IPv4 address still gets
whatever the network offers it over IPv6.

**None of it is written down by the shell.** This is the second setting in the
column that is not, for the same reason the sound device is not: NetworkManager
is what remembers a network once it has been joined, every other program on the
machine reads that, and a shell with its own copy would be a second opinion about
it at every login.

**It is also the one page in the column that needs a daemon.** Everything else
the shell reaches for it reaches for directly — the kernel's backlight, whatever
sound server the session has, the connector's own colour pipeline. Wireless
cannot be: joining a network means speaking the four-way handshake against an
access point, which is a supplicant, and nothing in the kernel does it. A shell
that wanted Wi-Fi without depending on anything would have to become one, and
would then be fighting the one the machine already has running. So this page
talks to NetworkManager, and a session without it gets one row saying so —
exactly as a session with no sound server does on the page above.

Proxies, VPNs, hotspots and enterprise credentials are deliberately absent.
Those are not one press and a value, they are forms, and a console shell
offering half a form would be worse than one that says plainly that the network
it cannot join has to be set up elsewhere.

## System

`Settings > System` is the page about neither the picture nor the sound. It holds
five rows: **Startup category**, which is the column a session opens on,
**Picture-in-Picture**, which is what happens to a browser's floating video
window, **Clock**, which is whether the corner writes the time the way this
country does, **Button hints**, which is whether the start screen writes
[what its buttons do](shell.md#what-the-buttons-do) in its corner, and **System
information**, which is the page a console needs to be able to say what it is.

**Application scaling used to be the row this page existed for**, and it is
[under Display](#application-scaling) now, per screen. The argument for keeping
it here was that how large an interface has to be to be read is a fact about the
person rather than about a monitor — which is true, and is not the whole of it:
the person is a different distance from each of their screens.

Button hints is under System rather than under Appearance, which is the one thing
about its place worth arguing over. What it changes is not how the shell *looks*
but how much it says about itself — the same kind of answer as how large an
application is drawn, and not the same kind as an accent colour.

### Picture-in-Picture

```
Settings > System > Picture-in-Picture   >  On, medium, top right
                                            Picture-in-Picture  >  Off / On
                                            Size                >  Small / Medium / Large
                                            Placement           >  the four corners
```

A browser asked to put a video into picture-in-picture opens a small window for
it, and that window is titled `Picture-in-Picture` — the one string every
browser that has the feature agrees on. It cannot be recognised any other way:
the window belongs to the browser and calls itself by the browser's name, which
is also what the window the video came out of calls itself.

A window that answers to that title is taken out of the layout every other
window here is under. It is **not maximized**, it is **not given the keyboard**,
it is **not listed in the guide** as something to switch to, and it is drawn
**in front of everything the session has** — over a fullscreen game, over the
start screen, and over the guide, which is the one surface nothing else in this
compositor is allowed in front of. That is the whole feature: a window that is
still there while the user does something else.

It is still clicked on, exactly where it is drawn, which is how its own play
button is pressed — and it is looked for *in front of* everything else for the
same reason it is drawn in front of everything else: what is nearest the hand
and what is nearest the eye cannot be two different windows. A press on it never
takes the keyboard, though. Whatever the user was working in goes on hearing
every key, which is the whole point of a video parked out of the way. And the
application it belongs to is never put to sleep while it is on screen, however
completely the rest of that application is covered — the sleeper asks whether
anything of an application can be seen, and this can.

**Size** is three shares of the display's width — a sixth, a quarter or a third
— rather than a number of pixels: the same choice has to mean the same thing on
a laptop panel and on a television across the room. How *tall* the window is at
that width is the window's own business. The compositor asks it what shape it
wants to be, by sending it a configure carrying no size at all — which is
xdg-shell for *choose one* — and follows the answer, so a four-to-three video is
drawn four to three and a phone's video stood on its end is drawn standing on
its end. Until it answers, and for a client that never does, sixteen to nine
stands in.

The answer is the first size the client draws that it was not *told* to draw,
and it is listened for as long as the window floats. Both halves of that were
paid for. A client's opening move is very often not a window at all — Firefox
commits a single pixel before it has laid anything out — and a placeholder read
as a shape says *square*, which is a widescreen video in a square frame. And a
browser will put a video of another shape into the same window, which is a
second answer to a question that a single reading would have closed.

**Placement** is the four corners and only the four corners: that is what a page
driven by a controller can honestly offer, and every corner holds the window off
both edges by the same distance — the shell's menu radius, which is what every
shape in this session is spaced by. A second picture-in-picture opened while the
first is still up stands **below it in a column** from that corner, in the order
they started floating — two windows in one corner is one window with something
wrong with it, and the one already there does not move aside for the newcomer.

**A mouse moves it.** Eight logical pixels in from each edge of
the surround is a band that resizes the window, and where two of those bands meet
is a corner that resizes it both ways; everything inside them is the video, and
dragging there carries the window. The pointer says which is which — it takes a
resize shape over the edges and leaves the client's own cursor alone everywhere
else. Neither is a client's drag: nothing is asked of the browser and nothing is
told to it.

**A press inside the frame is that window's, and nothing else's.** It is drawn in
front of everything the shell owns, so it is pressed in front of everything the
shell owns — ahead of the start screen and the guide, which are on the layer a
press would otherwise be answered by first. Without that rule a click on a video
was a click on whatever the shell had underneath it, and the user got a row
pressed they never aimed at. It stops there whether or not the client wants it,
too: a point inside the frame that no surface answers is a point *nobody* hears
about rather than one the application behind hears about, and the frame is asked
rather than the surface, since the surround is this compositor's own paint and
lies in no client at all.

The one exception is a **menu of the shell's on screen**, and not only under the
panel: a press past a menu is how a menu is dismissed, and that press has to
reach the shell to do it. A panel that could not be got rid of by clicking beside
it would be worse than a video that ignores one click — and a press outside a
menu presses nothing and only closes it, so nothing is done that the user did not
ask for.

The middle of the window has two jobs at once, so it does both. The press reaches
the client the instant it happens, so a play button answers at once, and the drag
only starts if the hand then travels four pixels — at which point the client is
told the pointer *left*, which is what cancels the click it was in the middle of.
It is deliberately not sent a release: a release is a *completed* click, and on a
video's own play button a completed click is the video stopping because somebody
moved it out of the way.

A drag never restretches the video. The opening keeps the shape its client asked
for, so all eight handles scale the window and pulling one edge moves the other
dimension with it; the edges nobody is holding stay exactly where they were. It
cannot be pulled smaller than the smallest the layout draws, or larger than the
screen, or off it — and it stays on the screen it was opened on, as every window
here does.

**A controller moves it too, out of the guide.** A console has no pointer, so
there is nothing to put on the window and nothing to press it with — but there
is a moment when the user is plainly not using the application underneath, and
that is the moment the overlay is up. **With the guide open, the right stick
pressed hands the guide's own directions to the videos floating over it**, and
presses again to hand them back; Back does the same. Nothing is offered when
there is nothing floating on that screen, and the press then does nothing at
all — nor while something with buttons of its own is standing over the menu: a
board being typed on, a question being answered, a file being carried, or the
power dialog, which is modal and takes every key so that a choice about ending
the session cannot be answered by something meant for the menu behind it.

**And the menu says so.** The row of button pictures in the corner of the
overlay gains a *Picture-in-Picture* pair while there is something floating over
that screen, and becomes a row about the window once the directions are on one —
see [what the menu's buttons do](guide.md#what-the-menus-buttons-do), which is
where the three rows are set out. Without it the feature was reachable only by
somebody who already knew it was there: nothing named the press that goes to the
video, and nothing named what the buttons did once they had.

The selected window is marked by the compositor rather than by the shell, which
is forced and is the right way round: such a window is drawn in front of every
surface the shell owns, so a mark drawn by the shell would be behind the thing
it marks. Its hairline surround turns the session's accent and an accent glow
breathes out into the shadow around it, on the same one-and-four-fifths seconds
everything else the user is choosing between breathes at. The guide's own
selection stops breathing while the directions are elsewhere: it stays where the
user left it and stays lit, because they are coming back to it, but two things
pulsing side by side is two controls claiming one thumb.

**And the guide steps back while they are gone.** Three things at once, because
they are one thing said three ways — *the thumb is somewhere else*. The
selection stops breathing, as above. The **frame around the selected window card
goes**: that ring is the whole of what says *this card answers the next press*,
and while the directions are on a video it does not, so a lit ring around a card
the D-pad no longer reaches is the shell lying about where the user is standing.
And the menu itself dims, a little under two fifths of the way down, over about
a fifth of a second — far enough that which of the two halves is live is
answered from the corner of an eye that is on the video, and not so far that it
stops being readable, because it is still what the user is coming back to.

The windows in the deck are *covered* for that rather than faded. Everything
else in the menu is the shell's own drawing and a fade reaches it; the windows
are the compositor's, and the shell only frames them. Fading the frame alone
would leave the brightest thing in the menu — a live window, very often a moving
picture — at full strength while everything around it went quiet, which reads as
a fault rather than as a step back. So the same share of the same dark glass is
laid over each card instead, and the whole menu arrives at one brightness.

It is a position rather than a start time, in both directions, so a guide that
gets its directions back before it has finished stepping away comes forward from
where it is instead of snapping the rest of the way out first.

**The right stick then carries the window**, exactly as a mouse button held on it
does — the same arithmetic, the same limits, the same leaving of the column. It
takes hold by itself, without a button to hold down, and lets go when the stick
comes back to rest. The D-pad and the left stick walk between the videos on that
screen, by **where they are** rather than by what order they were listed in: they
stand in a column until somebody moves one, and after that they are wherever they
were put. A direction with nothing that way moves nothing, which is what pushing
into the end of any other list here does. Only that screen's, because the guide
is only ever on the screen being driven and a window belongs to the screen it
opened on; the videos on the other screen are reached by taking the guide there
with the shoulder buttons.

**The top face button raises the window's own menu**, which is the menu the right
button raises, about the same window, with the same rows. Everything else on the
pad goes on meaning what it means everywhere — the guide button is still the way
out of all of it, the shoulder buttons still move between displays, the volume is
still about the machine and the camera still photographs whatever is in front of
the user.

**One thing is allowed over such a window, and it is a menu about it.** A window
made large enough covers the very menu that offers to make it small again —
awkward with a mouse, and a dead end on a pad, where there is no pointer to find
an unseen row with. So a context menu is drawn in front of the floating windows
rather than behind them, and the pointer follows the picture: a press on the
panel is the shell's, and one beside it is still the window's.

**The menu gets a surface of its own** for this, a child of the display's, and
nothing else is drawn on it. That is the part that had to be learned. A panel is
rounded and a rectangle is not, so lifting the panel's bounding box out of the
shell's main surface laid four square corners of the start screen over the video.
Given a surface of its own the panel is exactly its own shape, everything around
it is transparent, and the video shows through to its edge. The compositor is
told which surface it is and nothing more: it never learns where the panel is or
what shape it has, and the surface's own input region is what decides whose a
press is, so what the eye finds and what the hand finds cannot drift apart.

The surface is a *subsurface*, so it commits with the display's own and no frame
can show the panel without the wash behind it or the wash without the panel. It
is made the first time a menu is opened on that display, and its buffer comes
back off when the last one goes — a session where nobody opens a menu pays
nothing for any of this.

**The right button raises a menu** — the same menu the rest of this session
raises, drawn by the shell, because a compositor cannot draw the shell's glass.
The compositor asks for it and is told what was chosen. Eight rows: *Move*,
*Resize*, *Realign*, *Full screen*, *Move to next display*, *Move to previous
display*, *Close*, and *Cancel* in a band of its own.

*Move* hands the window to the pointer: the pointer is put in the middle of it,
the window follows until the next click, and a click of any other button puts it
back where it was. *Resize* does the same from a corner — the one with the most
screen behind it, which is where the room to grow is — with the pointer put on
that corner so it follows the hand rather than the window jumping to it. Nothing
is held down through either, because the button that chose the row was let go of
before the window ever moved.

**On a controller those two rows are the same two acts with the stick instead.**
Nothing is warped, because there is nothing to warp: the window follows the right
stick from the moment the row is chosen, Accept leaves it where it ended up and
Back puts it back where it started. Resize is the only way to resize a floating
window with a pad at all, since the stick alone moves it.

**The directions move the window inside that mode**, rather than walking the
selection off it — walking it off is what they must not do, since it would leave
a window being carried by nobody, but doing nothing at all was the answer to
that only for as long as the one control here was a stick. A keyboard has no
stick. So a direction is a step: eight logical pixels for the first press, and a
key held down accelerates over about fourteen repeats to sixty-four, which
crosses a 1080p display in about three seconds from a standing start. A step
small enough to place a window to the pixel takes half a minute to cross a
screen and one large enough to cross a screen cannot place anything; the ramp is
what has both. It eases rather than climbing at a constant rate, and a different
direction or a gap longer than a held key's starts the run again at walking
pace.

A resize pulls **the corner the compositor chose** — the one with the most room
behind it — so the direction that makes a window in the bottom-right larger is
up and left. That is the stick's behaviour exactly, and deliberately: which
corner is held is a fact about where the window is standing on a display only
the compositor has laid out, and a shell that mapped the keys some other way
would be guessing at it.

*Realign* undoes both of them: the window goes back to the size the Settings
page asks for, in the corner it asks for, wherever it had been dragged or pulled
to — and it springs there rather than snapping, the same spring the column moves
on when a video arrives in it or leaves it. A window already standing in the
column is already all of that, so the row simply confirms it.

*Full screen* takes the window out of the corner altogether and gives it the
display, as an ordinary application window: **maximized, listed in the guide,
holding the keyboard, closed and switched to like every other one**. It grows out
of its corner to get there, over the same three-tenths of a second and along the
same flight a window makes when the guide brings it back from a tile, because
what the row asks for is this window opened full screen and that is what opening
one full screen looks like here. The flight waits for the client: what it grows
*from* is the corner, and what it grows *to* is the window at its new size, so it
begins on the first frame the client has actually drawn that size rather than on
the frame it was told to.

**What sends it back is on the menu of the deck it has just joined.** The
guide's own window menu — the one raised on a card, in the table of
[context menus](guide.md#the-context-menu) — carries *Open as Picture-in-Picture*, which
is this row read the other way: the window leaves the layout and goes to sit in
the corner, with the surround, the size and the column a browser's video gets.
The two are one request in two directions, and what it carries is nothing more
than *which windows float*.

So it works on any window at all, which is the honest consequence of that rather
than a second feature: the user is a better judge of what belongs in a corner
than a title is. And what they say outranks the title from then on — a browser
goes on calling that window `Picture-in-Picture` after they have said they want
to watch it properly, and a session that read the title again would put the video
straight back in the corner it had just been taken out of.

The row is drawn only while this feature is switched on, and absent rather than
greyed when it is not: a window given a corner on a session with no floating
windows would have no menu to be got out of the corner with. That is the one
thing a switched-off Settings page has to keep true.

The two display rows send the same request the guide's own window menu sends,
and follow the same rule: neither wraps, and the row that cannot be taken is
disabled and still drawn, so the menu keeps its shape on every screen. A move
between screens changes the screen and nothing else — a window still standing in
the corner joins the new screen's column, and one that had been dragged keeps
the place it was put in. *Close* asks the window to close, which is what puts
the video back in the page it came from; it is deliberately not the Close the
guide offers for an application, because that one ends the application and the
application here is a browser with the rest of somebody's session in it.

**A window that has been moved leaves the column.** It keeps where it was put, it
is no longer counted in the corner it came from, and the place it used to stand
in is free — the next video that starts floating takes it, and a window below it
moves up into it exactly as it would have if the window had closed. Two things
put a moved window back: the *Realign* row above, which is one window saying so,
and the Settings page, which is all of them — choosing a size or a corner there
is the user saying where they want their videos, and every floating window
returns to the column when they do.

The window is drawn with a hairline surround — **three pixels, fixed** — and a
shadow, its corners rounded at eight. The surround is not decoration. A client's
buffer is a rectangle with four square corners and nothing in a renderer can cut
a curve out of one, so the corners are *covered* rather than cut: the window is
configured to the opening in the surround, and the surround is painted over its
edges — rounded on the outside at the full radius, rounded on the inside at what
is left of it.

**That is why the two numbers are one decision.** The client's square corner sits
√2·(r − b) from the centre of the outer arc, so it is hidden only where
√2·(r − b) ≤ r: a surround as thin as three pixels can only round a corner about
eight, and rounding it further at that thickness brings the four corners of the
client's buffer out through the curve. It is the shell's menu radius that this
window is held *off the edges of the screen* by, not what its own corners are
rounded at — those were one number while the surround was a share of the radius,
and a hairline frame would have parked the window three pixels off the edge of
the display. The arithmetic is in `crates/lxb-protocol/src/pip.rs`, shared with
the compositor that paints it so the two cannot disagree about the shape.

**Nothing is drawn outside that shape.** Two different clients make that worth
saying. One draws its own decorations — Firefox's picture-in-picture window is a
GTK window with its own rounded corners and its own drop shadow, spilling tens
of pixels outside the window proper, which without this hangs out past the
surround and is exactly the leak the surround exists to prevent. The other
simply does not take the size it is given, or has not taken it yet. So the
window is scaled down to fit its opening if it is drawing larger than one —
shrink only, the way the overview scales a window into a card — and then clipped
to it.

**And nothing shows through it.** The surround is painted a shade *over* the
picture, the way a mat lies on the edge of a photograph rather than beside it,
because nothing at that edge lines up: the opening is a fractional rectangle —
a quarter of the display's width inset by a third of a radius that is a share of
its height — while the client is configured at a whole size, its buffer lands on
whole pixels, and the surround's own edge softens itself over the one pixel it
falls in. Butt those together and half a pixel belongs to nobody, which on screen
is a one-pixel line of whatever the user is really doing, down one side of their
video. Half a pixel is enough, now that both halves take the opening from the
same arithmetic: it is what a fractional display scale leaves in the resampling,
and it antialiases the surround's inner edge the way its outer edge always was.

Behind the window, the opening is filled with the surround's own colour, for the
other way a hole opens: a client that does not cover it at all — one still
starting up, one that will not take its size, one with transparent corners of its
own — is centred and letterboxed on more surround. What is behind a floating
window is the application the user is actually using, and any of it seen *inside*
the frame reads as the window being broken rather than as something showing
through.

**It arrives and it leaves.** A window that simply appeared in a corner at full
size would read as a glitch rather than as something the user asked for, so it
fades up out of nothing over 240 ms while it grows the last tenth of the way to
its own size — eased out, so it is already travelling when the eye finds it and
slows into place. Going takes 180 ms and is the same two numbers read the other
way: it fades out and falls back a tenth, smoothstepped, so the last frame drawn
of it is worth nothing at all rather than a fifth of a window blinking off.

The frame, the picture and the colour behind the picture are one shape and go
through **one** transform about one origin — the middle of the window — rather
than three that agree by arithmetic; a surround three pixels thick has nothing
to spare for two halves rounding a corner to different pixels. What it costs is
a pixel: while the window is being scaled, the client's own drawing is cut one
physical pixel inside its opening, so that a pixel lost to rounding is a pixel
of *surround* over the video rather than a square corner of video outside the
curve. What shows in its place is the surround's own colour, which is what is
behind the video anyway.

**And the column springs.** Two things move a floating window that nobody is
dragging: another window arriving under it or leaving above it — which is what
happens the moment somebody pulls one out of the column, and the column closes
up behind it — and a press on the Settings page changing how large these
windows are or which corner they sit in. Both take 480 ms and both are a
*spring*: a decaying cosine that goes about a tenth of the way past its
destination, comes back about a hundredth short of it, and lands. One good
bounce and the ghost of a second, which reads as something soft rather than as
something sprung.

It lands *exactly*. The wobble is three and a half half-turns because a cosine
is exactly zero there, so the curve arrives on its destination at the end of the
480 ms rather than a fraction of a percent short — and a fraction of a percent
of a five-hundred-pixel window is two pixels appearing from nowhere on the last
frame of the animation, which is the jump the spring exists to remove.

The window's own picture is scaled per axis while it springs, so a window
springing into a shape of another proportion squashes on the way. That is the
same one transform everything else goes through, so the surround, the video and
the colour behind the video cannot come apart.

Three windows never spring, and each for its own reason. One with a **hand on
it** — dragging or resizing — is where the hand is, and only while the hand is
still on it: a drag cancelled puts the window back the way everything else
moves. One still **arriving** is already animating, and a browser answering what
shape it wants to be a few frames in must not turn that into two animations at
once. And one being laid out for the **first** time has nowhere to spring from.

Leaving is the harder half, and it is worth saying why. **A browser does not
stop calling its window picture-in-picture when the video goes back into the
page — it destroys the window.** A destroyed surface has no buffer, no texture
and no state, so a fade drawn from the window itself would have nothing to draw:
the video would vanish on the first frame and the surround would fade out around
a hole. So the compositor keeps a note of what each floating window last looked
like — the texture handles the renderer already made, which outlive the client
that gave them, and the numbers that say where on the screen they go — refreshed
every frame at the cost of a reference count per surface, and nothing at all on
a session with no video parked in a corner. When the client goes, that note is
what is faded out. A renderer that did not take the note draws the surround
fading with nothing inside it, which is the case on a second GPU and nowhere
else.

**Off** is the honest answer for somebody who does not want a video following
them around: such a window becomes the application window it otherwise is —
maximized, listed, focusable — which is what this session did before it could be
asked. It is written to `~/.config/lxb/shell.toml`:

```toml
picture-in-picture = true
picture-in-picture-size = "medium"
picture-in-picture-place = "top-right"
```

Carried out by the compositor, which is what places windows, and remembered
nowhere else: the shell says what this is as soon as it connects, which is long
before any browser exists to put a video in — the same bargain the application
scale above it is under.

### System information

The last row of the page opens a panel rather than a column: the shell's own
fennec mark over nine named facts about the machine, read at the moment it is
pressed and dismissed with `Close`.

```
Settings > System > System information

    System name        Some System
    System version     9
    System software    Version 0.1.0
    IP address         192.0.2.17
    Kernel             Linux 6.6.0-generic
    Processor          Some Core X9-9000 6-Core
    Graphics           Some Radeon X9000
    Memory             20 GiB free of 30 GiB
    Disk space         120 GiB free of 233 GiB
```

**System name** and **System version** are `NAME` and `VERSION_ID` out of
`/etc/os-release`; a rolling release publishes no version, and that row is left
off rather than shown empty. **System software** is this shell's own version —
the `VERSION` file at the root of the checkout, which both halves are built
against. **IP address** is the first ordinary address on an interface that is up
and is not the loopback, asked of the kernel rather than of a network manager,
since a session with no desktop in it has nobody to ask; a machine on no network
reads `Not connected`. **Kernel**, **Processor** and **Memory** come from
`/proc`, and **Disk space** from the filesystem the system is installed on.
**Graphics** is the adapter the shell itself is drawing through, which is the one
thing here no file on the disk knows.

Nothing on the page can be changed, and nothing is invented: a value the machine
will not give reads `Unknown` rather than something derived from the value beside
it. The processor and the adapter are named as their vendors name them, less the
`(R)` and `(TM)` marks. Two more words come off, both of them a label repeating
itself at the cost of the end of the row: a trailing `Processor` on the CPU, and
the trailing bracket in which Mesa writes the driver and chip codename rather
than the card.

