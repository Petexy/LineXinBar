# Project overview

[Documentation](index.md) · [Project home](../README.md)

- [Why not just Gamescope](#why-not-just-gamescope)
- [Not implemented](#not-implemented)
- [License](#license)

A micro Wayland compositor with first-class multi-display support, plus a
console-style lattice shell that runs inside it.

Two binaries:

| Binary        | What it is                                                    |
| ------------- | ------------------------------------------------------------- |
| `lxb`         | The compositor. DRM/KMS on a TTY, or nested inside a desktop.  |
| `lxb-desktop` | The shell: a console-style lattice launcher, on the GPU.       |

## Why not just Gamescope

Gamescope is single-output by construction: it owns one CRTC and scales one
application onto it. LineXinBar keeps the same deliberately small "one
application fills the screen" model, but every connected display is a real
output with its own CRTC, scanout swapchain and vblank-driven render loop.
Displays with different resolutions and refresh rates therefore run
independently rather than being locked to a shared heartbeat.

## Not implemented

Worth knowing before you rely on this:

- **A see-through overlay without a blendable surface.** The overlay asks for
  a premultiplied-alpha surface so the running application shows through it.
  A driver offering only `Opaque` gets a working menu on a solid background
  instead, and says so in the log.
- **Sharing one window rather than a whole display.** The portal offers
  displays only. A window's pixels can be photographed (`capture_window`) but
  not streamed, and `AvailableSourceTypes` says so rather than offering it and
  failing.
- **Gesture navigation in the shell.** [Mouse and touch](controls.md#mouse-and-touch)
  answers clicks, taps and the wheel; there is no swipe, pinch or two-finger
  handler on the bar. The compositor forwards pointer gestures to clients
  normally.
- **Unlimited relative-pointer capture in the nested debug backends.** They
  synthesize relative events from the parent cursor and enforce client locks,
  but movement stops at the outer window edge because Smithay's nested event
  adapters do not expose the parent's raw-motion stream. The native `udev`
  backend used for a dedicated LineXinBar/Steam Deck session receives true
  libinput relative motion and is not edge-limited.
- **Client colour management.** HDR is an *output* setting: the compositor
  drives the connector in BT.2020/PQ and re-encodes its own sRGB output to
  match. There is no `wp_color_management_v1`, so an application cannot hand
  over HDR content of its own — a game rendering in scRGB or PQ still submits
  an sRGB buffer and is displayed as SDR content on an HDR signal.
- **Variable refresh rate.** `adaptive_sync` is parsed from the config but not
  yet applied.
- **Signing Valve's client out.** The shell signs it *in* by calling the method
  the client's own login screen calls. There is no matching call for signing
  out: the client's own is `SignOutAndRestart`, and the restart puts its login
  window on the screen, which is the one thing this must never do. So signing
  out of the shell stops the client and clears the two files that would sign it
  back in, but leaves Valve's own cached credential alone — guessing at an
  unpublished format is how somebody's Steam configuration gets corrupted.
  Someone who then starts Steam **by hand** may find it still signed in, and
  signs out from inside it as they always would. The row is called **Sign out
  of LineXinBar** for that reason, and the panel behind it says so and offers
  to open Steam to finish the job.
- **Playing without Valve's client.** There was a version of this that started
  games itself, answered their Steamworks calls with its own library and fetched
  content out of Valve's depots. It worked, and it broke on every game that did
  anything unusual — a loader that opens `libsteam_api.so` by name, a Windows
  game whose prefix Steam had already made, an anti-cheat that wants the real
  client's pipe. It is gone. A machine with no Steam client can sign in and see
  its library, and can start nothing in it.
## License

GNU General Public License v3.0 only. See [LICENSE](../LICENSE).
