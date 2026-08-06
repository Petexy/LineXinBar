# Configuration

Linboard reads `$XDG_CONFIG_HOME/linboard/config.toml`, falling back to
`~/.config/linboard/config.toml`. A missing file is not an error: the defaults
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
| `shell`         | string            | `"linboard-xmb"`   | The session shell started by `--shell`. Consulted only when that flag is given. |
| `output_layout` | `horizontal` \| `vertical` \| `mirror` | `horizontal` | How outputs without an explicit `position` are arranged. |
| `output_gap`    | integer           | `0`                | Logical pixels inserted between auto-placed outputs. |
| `background`    | `[r, g, b, a]`    | `[0.02, 0.02, 0.04, 1.0]` | Colour behind everything, components in `0.0..=1.0`. |
| `draw_cursor`   | boolean           | `true`             | Draw the compositor's own cursor. Turn off when nesting inside a compositor that already draws one. |
| `cursor_theme`  | string            | *(unset)*          | XCursor theme for the pointer. Unset falls back to an inherited `XCURSOR_THEME`, then to the bundled Bibata Modern Classic. |
| `cursor_size`   | integer           | *(unset)*          | Nominal cursor size in logical pixels. Unset falls back to `XCURSOR_SIZE`, then 24. |
| `env`           | table of strings  | `{}`               | Environment variables exported to every child process. |

`mirror` puts every output at the origin, so they all show the same region.

The resolved cursor theme, size, and search path are exported as
`XCURSOR_THEME`, `XCURSOR_SIZE`, and `XCURSOR_PATH` to every child process, so
applications drawing their own pointer match the compositor's. Linboard ships
a subset of [Bibata Modern Classic](https://github.com/ful1e5/Bibata_Cursor)
under `share/icons/` (found relative to the binary in both a build tree and an
installed prefix), and compiles the default arrow into the binary as a last
resort, so there is always a visible pointer.

`shell` differs from `autostart` in that Linboard supervises it: the session
ends when it exits, and failing to start it is fatal rather than leaving a
compositor with nothing on screen. A bare program name is looked for next to
the `linboard` binary first, so a build tree runs its own matching shell.

`WAYLAND_DISPLAY`, `WAYLAND_SOCKET`, `DISPLAY`, and
`LINBOARD_XWAYLAND_DISPLAY` are session-boundary variables rather than
configurable child environment. After applying `general.env`, Linboard points
`WAYLAND_DISPLAY` at its own socket, removes `WAYLAND_SOCKET`, and either
removes `DISPLAY` or replaces it with its private XWayland display. This keeps
a nested session from leaking clients to its host.

Linboard also identifies children as a Wayland session through
`XDG_SESSION_TYPE=wayland`, `XDG_CURRENT_DESKTOP=Linboard`,
`XDG_SESSION_DESKTOP=Linboard`, and `DESKTOP_SESSION=linboard`. Activation
tokens inherited from the outer compositor are removed because they are not
valid in the inner session.

D-Bus is a separate activation boundary. Run the complete session through
`env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET -u DISPLAY LINBOARD_PRIVATE_DBUS=1
dbus-run-session -- linboard --shell` (the nested helper already does).
The marker lets Linboard safely replace that private bus daemon's activation
environment once its Wayland and XWayland sockets are ready; Linboard never
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

Refresh rates are matched to the closest mode the hardware reports, so `@60`
will select a 59.94 Hz mode. If the requested resolution does not exist at
all, the connector's preferred mode is used and a warning is logged.

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
combination. Unparseable bindings are logged and skipped rather than
aborting startup.

Letter keys name the physical key, so `Super+Q` matches Q pressed without
shift and `Super+Shift+Q` is a separate binding.

`guide` is the console "home" button. It is a compositor binding because a
fullscreen application holds the keyboard, so the shell would never see the
key itself; the compositor forwards it over `linboard_shell_v1`. The defaults
are `Super+G`, `Super+Home`, and `XF86HomePage`. It does nothing when no shell
has bound that protocol.

When running nested for debugging, the host compositor's own global shortcuts
win: KDE claims most `Super`+letter combinations, so pick something it does not
use, or drive the overlay with Escape inside the shell instead.

### Built-in defaults

| Binding | Action |
| ------- | ------ |
| `Ctrl+Alt+BackSpace` | `quit` |
| `Ctrl+Alt+F1`…`F12`  | `vt:1`…`vt:12` |
| `Super+Q`            | `close` |
| `Super+G`, `Super+Home`, `XF86HomePage` | `guide` |
| `Super+Tab`          | `cycle-window` |
| `Super+Left` / `Super+Right` | `focus-prev-output` / `focus-next-output` |
| `Super+Shift+Right`  | `move-to-next-output` |

## Example

```toml
[general]
shell = "linboard-xmb"
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
"Super+E" = "spawn:linboard-xmb"
"Super+Shift+Q" = "quit"
```

## Per-application settings

`$XDG_CONFIG_HOME/linboard/apps.toml`, written by `linboard-xmb` rather than by
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
