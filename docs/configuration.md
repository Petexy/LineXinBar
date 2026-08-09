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

D-Bus is a separate activation boundary. Run the complete session through
`env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET -u DISPLAY LXB_PRIVATE_DBUS=1
dbus-run-session -- lxb --shell` (the nested helper already does).
The marker lets LineXinBar safely replace that private bus daemon's activation
environment once its Wayland and XWayland sockets are ready; LineXinBar never
modifies an inherited host bus.

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

Anything defined here replaces the built-in binding for the same key
combination, with one exception: the `guide` chords below cannot be taken over
by another action. Unparseable bindings are logged and skipped rather than
aborting startup.

Letter keys name the physical key, so `Super+Q` matches Q pressed without
shift and `Super+Shift+Q` is a separate binding.

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
reads from `/dev/input` itself.

The chords also outrank every other binding, and cannot be bound to anything
else: the guide is the way back out of whatever is running, so a configuration
file that took its key for something else would leave a session with no way
home. Binding `guide` to a further chord adds it to the protected set rather
than moving it — `"Super+K" = "guide"`, say, makes that chord the home button
too and takes it away from the on-screen keyboard.

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
