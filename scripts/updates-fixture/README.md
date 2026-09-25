# Fake native tools for looking at Settings > Updates

`bin/` stands in for the programs the update coordinator runs, so that the
page, the review, a running transaction with its prompts, and the results can
be looked at in a nested session without a package manager, fwupd or the
network being touched. Put it first on `PATH` — `scripts/updates-shot.sh`
does — and the test coordinator discovers a system with four repository updates,
two Flatpak scopes with an update each, and three fwupd devices of which one
(an NVMe drive) is eligible while the other two are the exclusions the page is
meant to show.

Each fake honours its tool's unattended flag as the real one does —
`flatpak update -y`, `fwupdmgr -y` — and the coordinator always says it, so no
fake asks "Proceed?" in a run any more. The one prompt left is the fake
`fwupdmgr`'s password with echo off, which the real `fwupdmgr` never asks (it
asks polkit): it stands in for whatever unexpected question a native tool can
still put to an unattended run, which is the case the response field exists for.
A prompt waits `LXB_FIXTURE_WAIT` seconds — or the number in
`$HOME/fixture-wait`, or 3 — for an answer and then carries on, so a run
always finishes; a real tool waits for ever, which is right for it and
useless for a script. The scratch-home file holds the prompt long enough to photograph its field.

To put a polkit question over the running panel, hand `updates-shot.sh` a
third argument — a command run inside the session — that asks the nested
shell's own agent: `pkcheck --action-id org.freedesktop.policykit.exec
--process $(pgrep -n -f 'lxb-desktop --debug-actions') -u`, started after
Update now and killed a few seconds later. The panel is set aside for the
question and comes back when it is withdrawn. Never type a password into
it: the tally is the user's own account.
Every call is appended to `LXB_FIXTURE_LOG`, or to `$HOME/fixture-calls.log`
when that is unset.

**Update LineXinBar** appears once the fake `pacman` reports a shell installed
from a release page: write `$HOME/fixture-family` in the scratch home
(`lxb-compositor 0.9.0-1` and `lxb-desktop 0.9.0-1`, one a line) before the run.
`pacman -Q` then lists them, and `pacman -Qqm` says no repository offers them.
`updates-shot.sh` points the check at `releases/`, where LineXinBar has
"released" `v0.9.2-alpha`. The step that would install it is refused like every
root step here, because the helper that would run as root is the test's own
binary, so nothing is downloaded.

Two things cannot be faked, by design. A root step runs only a root-owned
executable through a root-owned `pkexec`, so `pacman -Syu` is refused with
"is not protected against replacement" before either is reached — which is
the failure path the system source shows in these runs, and the reason the
fake `pkexec` here is a guard rather than a stand-in. And the shell's
on-screen keyboard takes the keyboard when a response field opens — as it
does by itself when `fwupdmgr` here turns its echo off for `Password:` — so
a scripted `launch` then presses the board's highlighted key rather than
Send; the typed round trip is proven by the coordinator's integration test
instead. Full output is one press from the panel, whatever the panel is
showing; its terminal frame is scrolled with `left` and `right` from a pad,
Page Up and Page Down from a keyboard, and End returns to the live output.

## Protected-job implementation

The production coordinator requires a real inhibitor and an installed trusted
helper, so this harness now starts the integration test's `fixture_daemon` entry
point with an injected fake guard. No production flag bypasses authorization or
power protection. `scripts/updates-shot.sh` builds that test executable and stops
only its own process. Real root steps still fail protected-path validation.
The routine running view has **Full output** and **Run in background**; there is
no Details view to go through, and manual responses are offered on the panel
when it recognises a question and in Full output whether it does or not.
Screenshots are layout tests; native authorization and logind need isolated VM
validation.
