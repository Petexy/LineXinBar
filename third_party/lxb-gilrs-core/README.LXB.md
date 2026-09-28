# What this is, and what was changed

`gilrs-core` 0.6.8 from crates.io (<https://gitlab.com/gilrs-project/gilrs>,
commit `07e286e24b046cf39e5c367daa2770b805a64692`), Apache-2.0 or MIT, carried
in this tree rather than depended on from the registry. `LICENSE-MIT` and
`LICENSE-APACHE` are upstream's, from that commit; the published crate does
not include them.

**The directory and the package are named `lxb-gilrs-core` so that nothing — a
lock file, `cargo tree`, a vendored source archive, a packager reading the spec
— can take this for the published crate. It is not.** The library it builds is
still `gilrs_core`.

What this copy does that 0.6.8 does not:

- **A hot-plug event is never left unread behind another.** On Linux, a thread
  watches udev and passes each joystick that arrives or goes over a channel,
  writing an eventfd each time to say so. The eventfd is registered
  edge-triggered, and `handle_hotplug` returns as soon as one message has made
  an event, leaving the rest of the channel alone. Two devices changing inside
  one poll were one edge, so the second waited for some later, unrelated
  hot-plug to be read — and if none came, for ever. `next_event_impl` now asks
  the channel first on every call, in `src/platform/linux/gamepad.rs`, marked
  `LineXinBar:`.

  Why it mattered here: the shell's pad guard (`crates/lxb-desktop/src/pad_guard.rs`)
  takes each pad away and gives back a copy, so turning a pad off removes two
  devices within a millisecond. The copy's removal was left unread; when the
  pad came back, GilRs opened the grabbed original, which is silent, and never
  opened the new copy — the shell stopped answering a controller that every
  game could still read. Measured with `uinput` pads before the fix: polled
  every 8 ms as the shell polls it, one run in three ended exactly like that;
  left unpolled while both halves went, so that the two events arrived
  together, GilRs still counted the copy as connected every time, three runs
  of three. `pad_guard::hardware_tests::a_pad_turned_off_and_on_again_is_read_again`
  in `lxb-desktop` is that second run, and passes only with this fix.

Upstream is not patched anywhere else. Anything under `src/` that is not named
above is 0.6.8 as published, and upstream's master has the same code, checked
at the commit above.
