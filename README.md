# Linboard

A micro Wayland compositor with first-class multi-display support, plus an
XMB-style shell that runs inside it.

Two binaries:

| Binary          | What it is                                                        |
| --------------- | ----------------------------------------------------------------- |
| `linboard`      | The compositor. DRM/KMS on a TTY, or nested inside a desktop.      |
| `linboard-xmb`  | The shell: a cross-media-bar launcher, drawn on the GPU.           |

## Why not just Gamescope

Gamescope is single-output by construction: it owns one CRTC and scales one
application onto it. Linboard keeps the same deliberately small "one
application fills the screen" model, but every connected display is a real
output with its own CRTC, scanout swapchain and vblank-driven render loop.
Displays with different resolutions and refresh rates therefore run
independently rather than being locked to a shared heartbeat.

## Building

Needs a Rust toolchain and the usual Wayland/DRM development libraries
(`wayland`, `libinput`, `libseat`, `libdrm`, `mesa`, `libxkbcommon`). On an
Arch-based system these come with `base-devel`, `wayland`, `libinput`,
`seatd`, and `mesa`. Install `xorg-xwayland` as well to run X11 applications
and `dbus` for an isolated application session bus; Linboard still starts as a
Wayland-only session when XWayland is unavailable.

```sh
cargo build --release
```

## Running

### Nested, for development

The compositor appears as an ordinary window in your existing session, so it
can be started and killed without touching a TTY.

```sh
./target/release/linboard --backend winit
```

It prints the Wayland socket it created. Point clients at it:

```sh
env -u WAYLAND_SOCKET -u DISPLAY WAYLAND_DISPLAY=wayland-1 foot
```

### Nested with several virtual displays

The winit backend can only ever open one window. To exercise the
multi-display paths without owning extra monitors, the X11 backend opens one
window per virtual output (Xwayland is fine):

```sh
./target/release/linboard --backend x11 --outputs 3 --window-size 800x600
```

Each window is a real output with its own position in the logical layout,
which clients see through `wl_output` and `xdg-output`.

### Native, on hardware

From a TTY, with `seatd` running (or logind):

```sh
./target/release/linboard --backend udev
```

Every connected connector becomes an output. `Ctrl+Alt+F1`…`F12` switch VTs.

`--backend auto` (the default) picks `winit` when a session is already
running and `udev` otherwise.

### As a session

```sh
linboard --shell
```

That is the whole thing, and it is what to run from a TTY. `--shell` starts
`linboard-xmb` and ties the compositor's lifetime to it, so quitting the shell
logs you out rather than leaving an empty compositor with no way out of it.
Failing to start the shell is fatal, for the same reason.

A bare program name is looked for next to the `linboard` binary before `PATH`,
so a build tree runs its own matching shell:

```sh
./target/release/linboard --shell
```

Set `general.shell` in the config to run something else. To have a display
manager offer it, install [`share/wayland-sessions/linboard.desktop`](share/wayland-sessions/linboard.desktop)
into `/usr/share/wayland-sessions/`.

For anything besides the shell, `general.autostart` and a trailing command
both still work:

```sh
./target/release/linboard -- foot
```

Those are unsupervised: the compositor keeps running when they exit.

For a complete desktop session, start the compositor on its own session bus:

```sh
env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET -u DISPLAY \
  LINBOARD_PRIVATE_DBUS=1 dbus-run-session -- \
  ./target/release/linboard --shell
```

The `scripts/run-nested.sh` development helper does this automatically.

Linboard pins autostarted processes to its own `WAYLAND_DISPLAY` and removes
any inherited host `WAYLAND_SOCKET` and `DISPLAY`. If its private XWayland is
available, `DISPLAY` is then replaced with that server's value. The development
`scripts/run-nested.sh` helper starts the shell through this same boundary.
This is particularly important for the X11 nested backend: its outer host
windows are unrelated to the private display offered to applications.
Before launching the shell, Linboard also updates its marked private D-Bus
daemon with those private display names. D-Bus-activated GUI applications
therefore enter Linboard instead of inheriting the outer desktop.

X11 applications are not merely redirected to a socket: Linboard owns the
XWayland process and its X window manager. X11 toplevels are tiled, rendered,
focused, closed, and moved between outputs through the same compositor paths as
native Wayland windows. Clipboard and primary-selection transfers work in both
directions.

## The shell

`linboard-xmb` binds `zwlr_layer_shell_v1`, so it is not tied to Linboard —
it runs on any compositor implementing layer-shell, which also makes it
debuggable on its own.

Applications come from `.desktop` files in the usual XDG search path and are
grouped into the categories Plasma's launcher uses: Settings, System,
Multimedia, Graphics, Internet, Office, Games, Development, Education &
Science, Utilities, and Other. Empty categories are hidden.

### Several displays

Each display gets its own bar, not a copy of one: they browse independently and
remember where they were. Only one takes input at a time — the others are
dimmed and drop the footer, so it is obvious which one the controller is
driving. `L1` / `R1`, or `Tab` / `Shift+Tab`, hand control to the next display;
the compositor's first output has it at startup, since Wayland has no notion of
a primary display.

An application launched from a display opens on that display, because the shell
names that display with `set_launch_output` before starting anything. It does
not leave the compositor to work it out from keyboard focus: focus has usually
moved on to something else by the time the new window maps.

Only the catalogue of applications and the running processes are shared. The
device, icon atlas, shaders and shaped-glyph cache are shared too, so a second
monitor costs one more swapchain rather than a second copy of everything.
Hotplug is followed in both directions; unplugging the display that had control
passes it to a neighbour.

| Input                                      | Action                    |
| ------------------------------------------ | ------------------------- |
| `←` / `→`, D-pad or left stick left/right  | Change category           |
| `↑` / `↓`, D-pad or left stick up/down     | Change application        |
| `Enter`, controller `A` or `Start`          | Launch                    |
| `Tab` / `Shift+Tab`, `L1` / `R1`            | Move to another display   |
| `Esc`, `Backspace` or controller `B`        | Open the guide overlay    |
| `Home`, controller Guide/STEAM button       | Open the guide overlay    |

Keyboard navigation also accepts the keypad arrows, WASD, and HJKL. Held
directions repeat after a short delay; the analogue stick uses a dead zone
with hysteresis so drift near its edge cannot rapidly change selection.

Controllers are discovered and hot-plugged directly through the Linux gamepad
API, using SDL-compatible mappings (including `SDL_GAMECONTROLLERCONFIG`, as
used by Steam). A pad the mapping database does not know still works: buttons
it cannot name fall back to their raw Linux codes, so `BTN_SOUTH` launches and
`BTN_EAST` goes back whatever the database thinks. Every button press is logged
at debug level with both its mapped name and its raw code, which is the fastest
way to work out what an unusual pad is actually sending:

```sh
RUST_LOG=linboard_xmb::controller=debug linboard-xmb
```

Controller initialisation failure is non-fatal and leaves the keyboard usable.
Pass `--no-gamepad` to skip controller discovery entirely.

Icons are resolved through the freedesktop icon theme spec, following
`Inherits` from `index.theme` and falling back to hicolor and
`/usr/share/pixmaps`. Both PNG and SVG are supported, including extensionless
absolute paths used by AppImages and non-standard theme directory layouts.
Broken high-priority theme entries fall through to inherited themes, and a
generic application icon is used when a desktop entry provides no usable icon.

## The guide overlay

A launched application fills the screen and owns the keyboard, so there has to
be a way back out of it that the application cannot swallow. Pressing the
controller's guide button, or the compositor's `guide` binding, brings up a
menu over whatever is running:

| Entry             | Effect |
| ----------------- | ------ |
| Resume            | Dismiss the overlay |
| Applications      | Show the bar over the running application, without closing it |
| Close *app*       | Ask the application to close, so it can prompt about unsaved work |
| Quit Linboard     | End the session |

The last two are offered only while something is running. Nothing else ends
the session: `Esc` opens this menu rather than quitting, so leaving is always
a deliberate choice.

The overlay is drawn by the shell, not the compositor, so it uses the same
renderer and fonts as the bar. It moves its layer surface to the overlay layer
with `Exclusive` interactivity and draws onto a transparent surface, which is
what lets the running application stay visible through the scrim.

There is one overlay, on the display holding control — the other displays carry
on showing their own bar. `L1` / `R1` and `Tab` / `Shift+Tab` work from inside
the menu, and it follows control to the next display; with more than one
display it names the one it is on.

Everything it says is about that display: it offers to resume or close the
application running *there*, and reads *Nothing is running* on a display that
has none, whatever is on the others.

Two input paths reach it, because neither alone is enough:

- **Controllers** are read straight from `/dev/input` rather than through
  Wayland, so the guide button arrives even while a game holds the keyboard.
  Every other control is still ignored in that state, so the bar cannot react
  behind a running game.
- **Keyboards** go to the focused application, so the shell would never see the
  key. The compositor therefore owns the `guide` binding and forwards it over
  `linboard_shell_v1` (see below).

## `linboard_shell_v1`

Layer-shell says nothing about either half of the problem above, so
[`crates/linboard-protocol`](crates/linboard-protocol) defines a small private
protocol generated from one XML file for both sides:

| | |
| ------------------- | ------ |
| event `guide`       | The compositor's guide binding fired. |
| event `output_foreground` | Title of the topmost application on one display, empty when none. |
| event `foreground`  | The same for the session as a whole. Superseded; not sent from version 3. |
| request `close_output_foreground` | Ask that display's application to close. |
| request `close_foreground` | The same for the session as a whole. Superseded. |
| request `quit`      | End the session. |
| request `set_launch_output` | Name the display new applications should open on. |

`output_foreground` is what lets the menu say *Close KWrite* and notice when an
application it started has exited. It is reported per display because the
overlay belongs to one display: labelling it from the session's topmost window
offers to resume, or close, something on a screen the user is not looking at.

A window is attributed to the display it covers most of, rather than every
display it touches — otherwise one spilling over an edge claims both.

`set_launch_output` exists because keyboard focus is the wrong thing to infer
the launch display from. Starting a second application from the guide hands
focus back to the *first* one long before the new window maps, so the new
window would join the old one's display. A shell drawing one surface per
display already knows the answer, so it says it outright — which also avoids
racing the round trip a focus change costs. The compositor treats it as a
preference and falls back to focus, then the pointer, then any output.

It is session-private and on the same trust footing as wlr-layer-shell: any
client of this compositor may bind it. The shell degrades cleanly without it —
on another compositor it falls back to signalling the process group of what it
started itself, and `Quit` simply exits the shell.

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
Wayland socket as the shell. It accepts an X11 display only through Linboard's
`LINBOARD_XWAYLAND_DISPLAY` marker; arbitrary host `WAYLAND_SOCKET` and
`DISPLAY` values are discarded. The nested helper also starts a private D-Bus
session, preventing ordinary D-Bus activation from forwarding a launch to an
outer-desktop process. Applications with their own profile-based remote IPC
can still reuse an existing instance while nested; test those with a separate
profile or from the intended dedicated Linboard login session, where no host
desktop instance is running.

The shell and private XWayland server are one session unit. If their XWM
connection is lost, Linboard exits instead of leaving the shell with a stale
`DISPLAY`; a production service manager can then restart the complete session.
Waiting for XWayland's display-ready signal has a five-second deadline and
falls back to a Wayland-only shell if the server is missing, broken, or never
becomes ready.

## Configuration

`$XDG_CONFIG_HOME/linboard/config.toml` (usually
`~/.config/linboard/config.toml`). Every field is optional; see
[`docs/configuration.md`](docs/configuration.md) for the full reference and
[`examples/config.toml`](examples/config.toml) for a commented sample.

## Default keybindings

| Binding                | Action                          |
| ---------------------- | ------------------------------- |
| `Ctrl+Alt+Backspace`   | Quit the compositor             |
| `Super+Q`              | Close the focused window        |
| `Super+G`, `Super+Home`, `XF86HomePage` | Show the guide overlay |
| `Super+Tab`            | Cycle windows on this output    |
| `Super+←` / `Super+→`  | Focus the previous/next output  |
| `Super+Shift+→`        | Move the window to the next output |
| `Ctrl+Alt+F1`…`F12`    | Switch VT (udev backend only)   |

Any of these can be overridden in the `[keybindings]` table.

## Architecture

```
crates/linboard-compositor/
  state.rs        global state, split so a render pass can borrow the
                  backend and the compositor state at once; session shell
  handlers.rs     Wayland protocol handler implementations
  outputs.rs      multi-display layout: positions, scale, transform, tiling
  input.rs        input routing, focus policy, keybindings
  focus.rs        common Wayland/X11 keyboard, pointer and touch targets
  shell_control.rs  compositor half of linboard_shell_v1
  xwayland.rs     private XWayland server's X window manager and selections
  render.rs       render element assembly, shared by every backend
  backend/
    winit.rs      nested, one window
    x11.rs        nested, one window per virtual output
    udev.rs       DRM/KMS, libinput, libseat, multi-GPU

crates/linboard-protocol/
  protocols/      linboard-shell-v1.xml, the single source for both sides
  lib.rs          wayland-scanner bindings, client and server behind features

crates/linboard-xmb/
  apps.rs         .desktop parsing and Plasma-style categorisation
  icons.rs        icon theme lookup, PNG/SVG rasterisation
  model.rs        the shared catalogue, and one cursor per display
  guide.rs        the overlay's modes and menu
  ui.rs           layout: model to quads and text runs
  gpu.rs          wgpu renderer, one atlas and two pipelines
  shaders.wgsl    animated backdrop, instanced quads
```

## Not implemented

Worth knowing before you rely on this:

- **A see-through overlay without a blendable surface.** The overlay asks for
  a premultiplied-alpha surface so the running application shows through it.
  A driver offering only `Opaque` gets a working menu on a solid background
  instead, and says so in the log.
- **Screen capture.** No `wlr-screencopy` or xdg-desktop-portal, so
  screenshots and screen sharing do not work from inside.
- **Pointer- or touch-driven shell navigation.** The XMB has no pointer,
  touch, or gesture navigation handlers yet. The compositor still routes those
  input types normally to other clients; keyboard and game-controller XMB
  navigation are fully supported.
- **Unlimited relative-pointer capture in the nested debug backends.** They
  synthesize relative events from the parent cursor and enforce client locks,
  but movement stops at the outer window edge because Smithay's nested event
  adapters do not expose the parent's raw-motion stream. The native `udev`
  backend used for a dedicated Linboard/Steam Deck session receives true
  libinput relative motion and is not edge-limited.
- **HDR, variable refresh rate, and per-output colour management.**
  `adaptive_sync` is parsed from the config but not yet applied.

## License

GNU General Public License v3.0 only. See [LICENSE](LICENSE).
