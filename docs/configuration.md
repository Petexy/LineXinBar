# Configuration

LineXinBar reads `$XDG_CONFIG_HOME/lxb/config.toml`, falling back to
`~/.config/lxb/config.toml`. A missing file is not an error: the defaults
are a usable single-display setup.

Pass `--config PATH` to use a different file.

Unknown keys are rejected rather than ignored, so a typo is reported at
startup instead of silently doing nothing.

The shell keeps its own file beside this one — see
[Per-application settings](#per-application-settings) at the foot of this page.
It is written by the shell rather than read from it, and is not part of
`config.toml`.

## `[general]`

| Key             | Type              | Default            | Meaning |
| --------------- | ----------------- | ------------------ | ------- |
| `autostart`     | list of strings   | `[]`               | Commands run once the private XWayland server is ready, or immediately in Wayland-only fallback mode. |
| `shell`         | string            | `"lxb-desktop"`    | The session shell started by `--shell`. Consulted only when that flag is given. |
| `output_layout` | `horizontal` \| `vertical` \| `mirror` | `horizontal` | How outputs without an explicit `position` are arranged. |
| `output_gap`    | integer           | `0`                | Logical pixels inserted between auto-placed outputs. |
| `background`    | `[r, g, b, a]`    | `[0.02, 0.02, 0.04, 1.0]` | Colour behind everything, components in `0.0..=1.0`. |
| `draw_cursor`   | boolean           | `true`             | Draw the compositor's own cursor when there is one to draw. Turn off when nesting inside a compositor that already draws one. |
| `cursor_theme`  | string            | *(unset)*          | XCursor theme for the pointer. Unset falls back to an inherited `XCURSOR_THEME`, then to the bundled Bibata Modern Classic. |
| `cursor_size`   | integer           | *(unset)*          | Nominal cursor size in logical pixels. Unset falls back to `XCURSOR_SIZE`, then 24. |
| `env`           | table of strings  | `{}`               | Environment variables exported to every child process. |

`mirror` puts every output at the origin, so they all show the same region.

`background` is what is behind everything **when there is no session shell**.
A `--shell` session never shows it: the compositor draws the shell's own
analytic wallpaper into every frame that has no session content in it, which
is the interval before the shell's first frame and the interval after its
last. Both used to be this colour, and this colour is very nearly black — a
login screen handing over to a black screen and a logout starting with one.
The palette comes from `~/.config/lxb/shell.toml`, or from a display manager
that hands the session its wallpaper clock; see `LXB_BACKGROUND_HANDOFF`
below. So does the material it is drawn in — the `theme-wallpaper` key, below
that.

The resolved cursor theme, size, and search path are exported as
`XCURSOR_THEME`, `XCURSOR_SIZE`, and `XCURSOR_PATH` to every child process, so
applications drawing their own pointer match the compositor's. LineXinBar ships
a subset of [Bibata Modern Classic](https://github.com/ful1e5/Bibata_Cursor)
under `share/icons/` (found relative to the binary in both a build tree and an
installed prefix), and compiles the default arrow into the binary as a last
resort, so a missing theme can never leave the pointer invisible.

Whether the pointer is on screen at all is a separate question, and not one
this setting answers: the session starts without a cursor and shows one only
once something moves it, hiding it again whenever a key or a controller button
is pressed. See [The cursor](../README.md#the-cursor).

`shell` differs from `autostart` in that LineXinBar supervises it: the session
ends when it exits, and failing to start it is fatal rather than leaving a
compositor with nothing on screen. A bare program name is looked for next to
the `lxb` binary first, so a build tree runs its own matching shell.

`WAYLAND_DISPLAY`, `WAYLAND_SOCKET`, `DISPLAY`, and
`LXB_XWAYLAND_DISPLAY` are session-boundary variables rather than
configurable child environment. After applying `general.env`, LineXinBar points
`WAYLAND_DISPLAY` at its own socket, removes `WAYLAND_SOCKET`, and either
removes `DISPLAY` or replaces it with its private XWayland display. This keeps
a nested session from leaking clients to its host.

LineXinBar also identifies children as a Wayland session through
`XDG_SESSION_TYPE=wayland`, `XDG_CURRENT_DESKTOP=LineXinBar`,
`XDG_SESSION_DESKTOP=LineXinBar`, and `DESKTOP_SESSION=lxb`. Activation
tokens inherited from the outer compositor are removed because they are not
valid in the inner session.

D-Bus is a separate activation boundary, and the same names have to reach it:
a service the bus starts on demand inherits nothing from whatever asked for it,
so a bus that has not been told what this session is starts the desktop portal
with no display and no desktop name — which is a session with no screen sharing
in it, silently. LineXinBar therefore replaces the bus daemon's activation
environment (and the systemd user manager's) once its Wayland and XWayland
sockets are ready, on a session that **owns the seat** — the DRM backend, which
is the machine's own session — and stops the portal again on its way out so the
next session starts one of its own.

Nested inside another desktop the host's bus is not LineXinBar's to rewrite.
Run the complete session through `env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET -u
DISPLAY LXB_PRIVATE_DBUS=1 dbus-run-session -- lxb --shell` (the nested helper
already does); the marker says the session has a bus of its own, and only then
is that bus's environment replaced. Without it, a nested session leaves the
host's activation environment — and the host's portal — alone.

### `LXB_BACKGROUND_HANDOFF`

Not a setting: a one-shot record a display manager may put in the session's
environment, saying which wallpaper was on screen when it handed over and how
far its clock had run. A session started without one still comes up on the
wallpaper — the palette from `shell.toml`, the clock from zero — so nothing
requires it.

It is read twice, and by design. The compositor reads it for the frames it
draws before the shell exists (and after it has gone), then passes the record
through **unaltered** to the session shell alone, which does the whole of its
validation: that it names a visual this shell draws, that it was sampled on
this boot, and that it is not stale. Nothing else in the session sees it —
neither XWayland, nor autostarts, nor the portal, nor an application launched
later — and an inherited copy is removed at every one of those boundaries.

The record is `key=value` pairs joined by `;`, ASCII, at most 1024 bytes:

```text
v=1;visual=lxb-wallpaper-v2;clock=linux-monotonic;boot=<boot id>;sample-ns=<n>;scene-ns=<n>;accent=<palette>[;theme=<Default|Simple>]
```

`sample-ns` is `CLOCK_MONOTONIC` when the record was written and `scene-ns` is
the wallpaper clock at that instant, so the far end advances one by the
difference to recover the other. `boot` is `/proc/sys/kernel/random/boot_id`,
which is what makes a monotonic sample meaningful to another process. `accent`
is one of the palette names Settings offers.

`theme` is the one optional field, and the one field whose absence means
something: it names the material the wallpaper was being drawn in — see the
`theme-wallpaper` key below — for the benefit of a reader that cannot consult the
account's own settings. Only the wallpaper's half of that setting; the marks the
shell draws are no part of a wallpaper, so the reader this is written for has no
use for them and the record has never carried them. The compositor drawing a bridge frame in front of a *login screen*
is that reader: it runs as the greeter's own account, and the settings of the
person whose wallpaper is on the screen are in somebody else's home directory.
Where the field is absent, or names a material this build does not have, the
reader falls back to the settings it can read. The session shell ignores it
outright — the shell is the account and has already read its own file — but it
accepts a record carrying one rather than refusing the phase over a field that
is none of its business.

Bump `visual` if the wallpaper
changes in a way that would draw a different frame at the same clock:
LineXinBar's own consumer refuses a visual it does not know, which is a
session that starts its animation from zero rather than one that jumps.

Console Experience Desktop Manager writes it. Any display manager can.

### `LXB_HOLD_DISPLAY`

Also not a setting. Set to `1`, `yes` or `true` by a display manager on both
sides of a login, it says that the displays are being passed between
compositors rather than given back to a console. Anything else, the variable
being absent included, means a session that ends by handing the machine back to
whoever started it.

It changes three things, all of them at the edges of a session and none of them
while one is running:

* **Coming up, the device is inherited rather than reset.** A compositor
  normally disables every connector and clears every plane on opening the GPU,
  so that no earlier compositor's state can make its own commits fail. That is a
  black screen for as long as it takes to reach the first frame — a fifth of a
  second here — laid over whatever the last compositor was still showing.
  Inheriting instead means the connector-to-CRTC mapping is recovered as it was
  left, so the first commit is a plane update on a live display rather than a
  modeset. If what was inherited turns out to be unusable, the device is reset
  and rescanned once, and the flicker comes back rather than the display
  staying dark.
* **Going down, the colour pipeline is left alone.** Undoing HDR makes the panel
  re-sync, which is a black screen of the display's own making arriving exactly
  where one is being removed. The compositor that follows sets its own within a
  frame. This is the same decision as keeping the night light across a
  hand-over, applied to the rest of the pipeline — so a display manager that
  sets this must be handing to a compositor that sets its own colour state, or
  the next session inherits BT.2020/PQ and does not know it.
* **Going down, the picture is held.** Closing the last handle on a DRM device
  is itself what blanks a display: the kernel destroys the framebuffers that
  file created, and removing one a plane is still scanning out disables the
  plane and the CRTC behind it. So a child process is forked to hold that
  descriptor open, and does nothing else with it, until the next compositor has
  committed a frame of its own or ten seconds have passed. Nothing is drawn, no
  device is held open beyond that, and a session that fails to start therefore
  leaves a machine that plainly needs attention rather than one frozen on a
  picture of a login screen.

Set it for a hand-over and only for a hand-over. On a bare TTY it would hold a
picture over the console a user is expecting back.

Holding the picture is necessary and is not sufficient. What the compositor that
follows does with the display it was handed decides whether anybody sees a black
screen — see below.

## `theme-wallpaper` and `theme-icons`, in `shell.toml`

`~/.config/lxb/shell.toml`, top level, beside `accent`:

```toml
accent = "Purple"
theme-wallpaper = "Default"
theme-icons = "Default"
```

How much material each half of the shell is drawn with. `Default` is its own
look: the wallpaper's current is a band of water three sheets thick, lit as
bodies, and every one of the shell's own marks is a bead of water shaded out of
its own distance field. `Simple` stands that down — the current becomes the three
fine glass-silk ribbons the shell drew before the band, and a mark becomes the
flat shape of itself in white, tinted with the accent. The *drawings* do not
change; only what they are made of does.

Two keys rather than one, and either may be either way round. They are separate
settings because they are separate expenses and separate tastes: the wallpaper is
one evaluation of a long function for every pixel of every screen on every frame,
and a mark is a few dozen pixels of a settings row. Measured on an RX 9060 XT,
one full-screen evaluation of the wallpaper is **0.38 ms** at 1080p in `Default`
and **0.20 ms** in `Simple`, and a mark goes from six reads of its distance field
to one. A machine that cannot pay for the first can very well pay for the second.

They are written from Settings > Appearance > Theme, which is a page with a row
for each. An unknown name is read as `Default`.

A file written before the setting was split carries a single `theme` key, which
said one thing about the whole shell. It is still read, and read as what it
meant — both halves take it where they have no key of their own — so a machine
deliberately stood down to `Simple` stays there across an update. It is never
written: the first save afterwards replaces it with the pair.

Both halves of a hand-over read these keys. The compositor reads
`theme-wallpaper` for the bridge frame it draws before the shell's first frame —
that frame is a wallpaper and nothing else, so `theme-icons` is none of its
business — and the display manager reads both for the login screen, which draws
its own marks. So a machine set to `Simple` is in `Simple` from the moment the
greeter appears, and never changes material in front of the user. A greeter
drawing for somebody else's account passes the wallpaper's answer along in the
hand-over record; see `theme` under `LXB_BACKGROUND_HANDOFF`.

## What the displays were last set to

`$XDG_STATE_HOME/lxb/displays.toml`, or `~/.local/state/lxb/displays.toml`.

Not a file to edit. The compositor writes it whenever the shell changes a
display, and reads it back before it lights anything.

It exists because of when the shell can speak. Every display setting arrives
over `lxb_shell_v1`, and the shell cannot send one until it has a Wayland
connection — about a second after the compositor has already brought the
displays up in whatever it had. So the compositor would light a display, and
then a second later be told to drive it in HDR instead. Turning HDR on or off
re-drives the pipe and the panel re-locks behind it: measured on an RX 9060 XT,
**169 ms** of dark for coming up in the wrong one, and **196 ms** more for being
corrected. Two black screens, either side of a second of un-warmed picture, on
every single login.

Remembering means the first commit already carries the right mode, orientation
and colour pipeline, so the driver finds nothing to change and cancels the
modeset. It is what makes a hand-over seamless rather than merely short, and it
is why `LXB_HOLD_DISPLAY` alone was not enough.

The file uses the same `[[output]]` shape as `config.toml`, and **`config.toml`
wins**, key by key. A key written by hand is a decision and is never overridden;
remembered state only fills in what the config leaves unsaid. So a machine that
pins `hdr = false` stays in SDR however often the shell is asked for HDR, while
one that pins only a resolution still comes up in the colour it was left in.

Deleting it costs one blinking login while it is written again. A file this
compositor cannot parse is reported and ignored, never repaired: it is more
likely to be a newer version's than a corrupt one.

## `[input]`

| Key                   | Type    | Default | Meaning |
| --------------------- | ------- | ------- | ------- |
| `keyboard_layout`     | string  | `"us"`  | xkb layout. |
| `keyboard_variant`    | string  | `""`    | xkb variant. |
| `keyboard_options`    | string  | unset   | xkb options, e.g. `"ctrl:nocaps"`. |
| `keyboard_model`      | string  | `""`    | xkb model. |
| `keyboard_rules`      | string  | `""`    | xkb rules. |
| `repeat_rate`         | integer | `25`    | Key repeats per second. |
| `repeat_delay`        | integer | `600`   | Milliseconds before repeat starts. |
| `tap_to_click`        | boolean | `true`  | Touchpad tap-to-click. |
| `natural_scroll`      | boolean | `false` | Reverse scroll direction. |
| `disable_while_typing`| boolean | `true`  | Ignore the touchpad while typing. |
| `pointer_accel`       | float   | `0.0`   | libinput acceleration, `-1.0..=1.0`. |

## `[[output]]`

Repeat this table once per display. Entries are matched by connector name
(`DP-1`, `HDMI-A-1`, `eDP-1`, …). The name `"*"` matches any output that has
no more specific entry; an exact match always wins.

Connector names are printed at startup, and `wayland-info` will list them
from inside a running compositor.

| Key             | Type              | Meaning |
| --------------- | ----------------- | ------- |
| `name`          | string            | Connector name, or `"*"`. |
| `mode`          | string            | `"2560x1440@144"`, `"1920x1080"`, or `"preferred"`. |
| `position`      | `[x, y]`          | Pins the output. Without it, `output_layout` decides. |
| `scale`         | float             | Fractional scale, e.g. `1.5`. |
| `transform`     | string            | `normal`, `90`, `180`, `270`, `flipped`, `flipped-90`, `flipped-180`, `flipped-270`. |
| `enabled`       | boolean           | `false` leaves the display unlit and out of the layout. |
| `adaptive_sync` | boolean           | Parsed, but not yet applied. |
| `hdr`           | boolean           | Drive this display in high dynamic range. Default `false`. |
| `hdr_sdr_brightness` | integer      | Luminance plain white is sent at, in cd/m². Default `200`. |
| `hdr_srgb_intensity` | integer      | How far sRGB colour is stretched towards BT.2020, `0`–`100`. Default `0`. |
| `hdr_peak_brightness` | integer     | Peak declared to the display, in cd/m². Omit to use the display's own. |
| `night_light`   | boolean           | Warm this display's picture. Default `false`. |
| `night_light_temperature` | integer | How warm, in kelvin — lower is warmer. `1000`–`6500`, default `4000`. |

Refresh rates are matched to the closest mode the hardware reports, so `@60`
will select a 59.94 Hz mode. If the requested resolution does not exist at
all, the connector's preferred mode is used and a warning is logged.

`mode` is what a display **comes up in**. The shell changes it at runtime from
Settings → Display → Resolution and → Refresh rate, and writes what it chose
per connector in its own file (`~/.config/lxb/shell.toml`), in this same
`WIDTHxHEIGHT@REFRESH` format. On a machine that boots into `lxb --shell`
the mode is therefore normally decided there; set it here for a session with no
shell, or to choose what a fresh profile starts at. A size the connector does
not list is refused whichever file asked for it, and the display keeps the mode
it has.

`transform` is the same arrangement for which way up the picture is drawn. The
shell changes it from Settings → Display → Orientation and writes it back under
this same key and these same spellings, so a line can be moved between the two
files and mean the same thing. The page offers the four rotations; the four
`flipped-*` values are here only, and a display set to one of them is named on
that page with none of its four rows marked.

Turning is the compositor's own drawing rather than anything the connector
does: the picture is composited turned and scanned out at the mode's own
pixels, so it needs no hardware support and works on any display this
compositor owns. What it changes is the display's *logical* size — a quarter
turn swaps its width and height, which moves the outputs laid out after it —
so `position`, `scale` and the window layout are all resolved against the
turned size.

`position` is also what takes a display out of the shell's reach. Displays
without one are laid out in the order they were plugged in, and Settings →
Display → Display order changes that order: choosing a place for a screen
trades it with the screen standing there, and the shell writes the whole
resulting arrangement under a per-connector `order` key in its own file
(`~/.config/lxb/shell.toml`, counted from one). A display pinned here is where
this file says and takes no part in it, so it is left off that page rather than
listed with a setting that would do nothing — as is every display when
`output_layout` is `mirror`, where the screens share one region and there is no
first one to be.

The arrangement is the one display setting the compositor does not remember
across a session. The rest are written to `displays.toml` because coming up in
the wrong one costs a modeset, which is a black screen; an arrangement costs a
relayout, and at login there is nothing laid out yet. So the shell keeps it and
sends it when it connects.

### High dynamic range

The four `hdr_*` keys are what the session **comes up in**. The shell writes
the same settings at runtime from Settings → Display → HDR → *the screen*, and
what it writes is remembered per connector in its own file
(`~/.config/lxb/shell.toml`), so a machine that boots into
`lxb --shell` is normally configured from there rather than from here. Set
them here for a session with no shell, or to choose what a fresh profile starts
at.

Turning HDR on does two things at once. The connector is told the signal is
BT.2020 with the ST 2084 (PQ) transfer function, through the `Colorspace` and
`HDR_OUTPUT_METADATA` properties; and the CRTC's colour pipeline
(`DEGAMMA_LUT` → `CTM` → `GAMMA_LUT`) is loaded to re-encode the composited
sRGB picture into that signal. Both are needed: a display told to read sRGB
numbers as PQ shows a scene that is far too dark and the wrong colour.

All five properties are set in **one** atomic commit, and that commit is first
offered to the driver as a `TEST_ONLY` request. A pipeline the hardware will
not take is therefore rejected by a commit that changed nothing, rather than
attempted on a live display; the log says so and the display stays in SDR.
Leaving the session puts every display back into SDR before giving up DRM
master, so quitting always returns a readable console.

HDR is refused, and logged, unless the driver is on the atomic interface and
the display's EDID advertises ST 2084 *and* the driver publishes
`HDR_OUTPUT_METADATA` and at least a `GAMMA_LUT`.

`hdr_srgb_intensity` needs a `DEGAMMA_LUT` as well, because it is implemented
as the `CTM` and a matrix is only a gamut conversion when it acts on linear
light. Without one the sRGB decode is folded into the gamma curve instead —
brightness stays correct — but the matrix is set to identity, which pins the
gamut at the vivid end whatever this key says: sRGB's primaries go out as
BT.2020's. Not all hardware exposes a degamma stage; amdgpu on RDNA 4, for
instance, does not on its display pipes. `lxb` logs which of these
happened when it drives a display into HDR — look for `pipeline=` on the
`driving this display in HDR` line — and tells the shell, which stops offering
the setting on displays that cannot honour it.

To see what a connector publishes before turning anything on, run with
`RUST_LOG=debug` and look for the `colour pipeline` line logged when each
display is connected. It reports whether the driver is atomic, which of the
five properties exist, and the entry counts of the two LUTs.

`hdr_sdr_brightness` is the setting that matters most, because nothing LineXinBar
composites is HDR content: every application and the shell itself are SDR, so
this alone decides whether the session comes out dim or blinding. sRGB's own
reference is 80 cd/m², which describes a darkened grading suite.

`hdr_srgb_intensity` decides what happens to colour inside BT.2020's much wider
gamut. At `0` sRGB's colours are placed where they actually belong, so the
session looks exactly as it did in SDR. At `100` the numbers are passed through
untouched, so sRGB's red is displayed as BT.2020's red and everything comes out
far more saturated — the "vivid" mode a television ships in. In between is a
blend of the two.

### Night light

The two `night_light_*` keys are the blue light filter, and they are what the
session **comes up in** for the reason the `hdr_*` keys are. The shell writes
the same setting at runtime from Settings → Display → Night light → *the
screen*, per connector in its own file (`~/.config/lxb/shell.toml`), so a
machine booting into `lxb --shell` is normally configured from there.

It is the CRTC's `GAMMA_LUT`: green and blue scaled down against red, so the
picture goes warm without anything getting brighter. That means it needs the
atomic interface and a gamma ramp and *nothing else* — no EDID claim, no
infoframe, nothing of what the link can carry — so unlike HDR it works on
essentially any display this compositor owns, including SDR laptop panels. A
nested session is the exception: it owns no CRTC, so there is no ramp to load.

The filter composes with HDR rather than competing with it. Both are encoded
into the same ramp and committed together, so turning HDR on does not undo a
warm picture and turning the night light on does not undo the PQ encoding. On
an HDR display the white point is applied in linear light, in front of the PQ
encode; on an SDR one the ramp decodes, scales and re-encodes sRGB. They are
the same white point expressed for two stages, so a display does not change
colour when it changes signal.

`6500` is ordinary daylight and is exactly the picture with the filter off: the
curve is normalised so that temperature is the identity to the last code, not
merely close to it. Below roughly 1900 K a black body has no blue in it at all
and the ramp takes that channel to zero, which is why the shell's own page
stops at 2000 K; a value set here is still honoured, and anything outside
`1000`–`6500` is brought to the nearest end rather than refused.

**There is no schedule here, deliberately.** Keeping hours means a clock and a
time zone, and the compositor owns neither — the shell works these out against
the machine's local time and sends only whether the light should be burning
now. That is also where the sunset-to-sunrise schedule lives, and where the
coordinates it needs are read: the shell's own file, not this one. So these keys
are an always-on setting: for a session with no shell, or for what a display is
warmed to until one connects.

### How large applications draw themselves

**There is no key here for it, deliberately**, although it is the compositor
that carries it out. Settings → System → Application scaling gives every
application a logical window some fraction smaller than the display and tells it
— over `wp_fractional_scale_v1` — to fill that window with the display's own
pixels, so an interface comes out larger without losing a pixel of sharpness.
The shell keeps the number, in its own file (`~/.config/lxb/shell.toml`), and
sends it over `lxb_shell_v1` as soon as it connects.

That is the whole difference from the two settings above. Those are here because
the compositor lights the displays a second before the shell can speak, and
being corrected afterwards costs a black screen. Nothing is on screen to
correct here: every application on the session is started *by* the shell, always
after it has said what this is, so no window is ever configured at the wrong
size in the first place.

The shell's own surfaces are not affected — it is not an application, and a
shell that changed size with this would take the Settings page being read with
it — and neither are windows under Xwayland, which have no per-surface scale to
be told about.

## `[keybindings]`

Keys are `Modifier+Modifier+Keysym`. Modifiers are `Ctrl`, `Alt`, `Shift`,
and `Super` (also spelled `Logo`, `Meta`, or `Mod4`), all case-insensitive.
The final component is an xkb keysym name such as `Q`, `F1`, `Return`, or
`BackSpace`.

Actions:

| Action                | Effect |
| --------------------- | ------ |
| `quit`                | Shut the compositor down. |
| `close`               | Ask the focused window to close. |
| `spawn:COMMAND`       | Run a command. Quoting follows shell rules. |
| `vt:N`                | Switch to VT `N`. Ignored when nested. |
| `focus-next-output`   | Move focus to the next output. |
| `focus-prev-output`   | Move focus to the previous output. |
| `move-to-next-output` | Send the focused window to the next output. |
| `cycle-window`        | Rotate the window stack on this output. |
| `guide`               | Show the session shell's guide overlay. |
| `keyboard`            | Show the session shell's on-screen keyboard. |
| `screenshot`          | Photograph the display the user is on. |
| `volume-up`           | Turn the session up one step. |
| `volume-down`         | Turn it down one step. |
| `volume-mute`         | Silence it, or bring it back. Also spelled `mute`. |

Anything defined here replaces the built-in binding for the same key
combination, with one exception: the `guide` chords below cannot be taken over
by another action. Unparseable bindings are logged and skipped rather than
aborting startup.

Letter keys name the physical key, so `Super+Q` matches Q pressed without
shift and `Super+Shift+Q` is a separate binding.

`Any+` in front of a key binds the key rather than a chord: it fires whatever
modifiers are held, and none of them can be spelled beside it. It is for a key
with one job printed on its cap, whose variants elsewhere are all the same job
here — `Any+Print`, so that `Shift+Print`, `Ctrl+Print` and `Meta+Shift+Print`
all take the one kind of picture this shell takes, and the three volume keys,
which mean the same thing however they are reached. Reach for it sparingly: a loose binding on a letter takes that letter
away from every application in the session, in every chord it appears in.
Binding the same key here, decorated or not, takes the whole key back from a
loose built-in — writing `"Print" = "spawn:grim"` leaves `Shift+Print` doing
nothing rather than still photographing the screen.

`guide` is the console "home" button. It is a compositor binding because a
fullscreen application holds the keyboard, so the shell would never see the
key itself; the compositor forwards it over `lxb_shell_v1`. The chords are
`Super+Home` and `XF86HomePage`. It does nothing when no shell has bound that
protocol.

Three more ways in are not chords at all and are therefore not in the table.
The **Windows key on its own** is the home button: it is watched for being
pressed and let go of with nothing in between, because a bare modifier is half
of every `Super+…` binding here and one looked up as a chord would shadow all
of them. Both edges of the key still reach the application, so nothing is left
holding a modifier it is never told about again. The **rear side button of a
mouse** does the same, and is held back from the application entirely, both
edges of it. So does a **controller's Guide or STEAM button**, which the shell
reads from `/dev/input` itself — on its release rather than its press, because
holding it with `R1` is the shell's screenshot chord and a modifier that also
acted on the way down could not be one. A tap is still a tap; a hold spent on
the chord opens nothing.

The chords also outrank every other binding, and cannot be bound to anything
else: the guide is the way back out of whatever is running, so a configuration
file that took its key for something else would leave a session with no way
home. Binding `guide` to a further chord adds it to the protected set rather
than moving it — `"Super+K" = "guide"`, say, makes that chord the home button
too and takes it away from the on-screen keyboard.

`screenshot` is a compositor binding for the same reason `guide` is — the
picture is of whatever is in front, so the key has to work while something is
in front of everything — but it is not protected, and any of its chords can be
given to something else. Its three spellings are the ones hands arrive already
knowing: the Print key under any modifiers, and the Mac chord transcribed onto
a PC keyboard both of the ways it gets transcribed, for a keyboard that has no
Print key at all. It photographs the display holding the keyboard,
writes it into the folder `xdg-user-dirs` records for pictures, and answers
with a flash of that display and the shell's `screenshot.ogg`. Unlike `guide` it
is a *round trip*: the compositor asks the shell where the file should go, so
the key does nothing at all when no shell has bound the protocol.

A controller reaches the same picture without going through this binding at
all. **Guide or STEAM held with `R1`** photographs the display holding control,
and it is the shell's own chord rather than the compositor's, because a pad is
not the compositor's to read: the shell opens it from `/dev/input`, which is
also why the chord works while a game holds everything else. Nothing in this
file changes it.

The three `volume-*` actions are compositor bindings for the reason `guide` and
`screenshot` are, and the plainest of the three: the fullscreen application
holding the keyboard is usually the thing being turned down. They are forwarded
over `lxb_shell_v1` and set nothing themselves — the shell owns the mixer, knows
where the control stands and has somewhere to draw it, so the key does nothing
at all when no shell has bound the protocol. The built-ins are the three keys
with a speaker printed on them, bound with `Any+` for the reason `Print` is: they
are reached through Fn on a laptop and through a media row on a keyboard, and no
two of those agree about what else is held down at the time.

Held down, `volume-up` and `volume-down` go on stepping at the `repeat_rate` and
`repeat_delay` set in `[input]`, because they are keys on the user's keyboard
like any other; `repeat_rate = 0` turns that off as it does everywhere else.
`volume-mute` never repeats: a switch held down is a switch thrown once.

When running nested for debugging, the host compositor's own global shortcuts
win: KDE claims most `Super`+letter combinations, so pick something it does not
use, or drive the overlay with Escape inside the shell instead.

### Built-in defaults

| Binding | Action |
| ------- | ------ |
| `Ctrl+Alt+BackSpace` | `quit` |
| `Ctrl+Alt+F1`…`F12`  | `vt:1`…`vt:12` |
| `Super+Q`            | `close` |
| `Super+Home`, `XF86HomePage` | `guide` (and `Super` on its own, which is not a chord) |
| `Super+K`, `XF86Keyboard` | `keyboard` |
| `Any+Print`, `Ctrl+Shift+3`, `Alt+Shift+3` | `screenshot` |
| `Any+XF86AudioRaiseVolume` | `volume-up` |
| `Any+XF86AudioLowerVolume` | `volume-down` |
| `Any+XF86AudioMute`  | `volume-mute` |
| `Super+Tab`          | `cycle-window` |
| `Super+Left` / `Super+Right` | `focus-prev-output` / `focus-next-output` |
| `Super+Shift+Right`  | `move-to-next-output` |

## Example

```toml
[general]
shell = "lxb-desktop"
output_layout = "horizontal"
output_gap = 0
background = [0.02, 0.02, 0.04, 1.0]

[general.env]
MOZ_ENABLE_WAYLAND = "1"

[input]
keyboard_layout = "pl"
repeat_rate = 30
repeat_delay = 400

# Primary display, pinned at the origin.
[[output]]
name = "DP-1"
mode = "2560x1440@144"
position = [0, 0]
scale = 1.0

# Secondary, placed to its right.
[[output]]
name = "HDMI-A-1"
mode = "1920x1080@60"
position = [2560, 0]

# Never light up the built-in panel.
[[output]]
name = "eDP-1"
enabled = false

[keybindings]
"Super+Return" = "spawn:foot"
"Super+E" = "spawn:lxb-desktop"
"Super+Shift+Q" = "quit"
```

## Per-application settings

`$XDG_CONFIG_HOME/lxb/apps.toml`, written by `lxb-desktop` rather than by
the compositor: it holds the choices the guide overlay makes about one
application, which have to outlive that application running.

Applications are keyed by the name they give themselves — an `xdg_toplevel`'s
`app_id`, or an X11 window's class — never by the window title, which is a
document name and would change under the setting.

```toml
[apps."org.mozilla.firefox"]
# The controller is a mouse inside this application: the right stick aims, `A`
# and `B` (or R3 and L3) click, and the left stick and D-pad scroll. Off unless
# the guide's tile has been switched on.
stick-pointer = true
```

Editing it by hand is fine. The shell reads it once at startup and rewrites it
whenever a setting changes, keeping only the applications something has been
chosen for; an unreadable file is reported and treated as empty rather than
being allowed to stop the session coming up.
