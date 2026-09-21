# Settings > Updates

[Documentation](index.md) · [Project home](../README.md)

Updates are grouped by their owner. The desktop offers **Update the system**,
**Update Flatpaks**, **Update Snaps**, independent Nix/Guix profiles, and
**Update firmware** according to what it detects. “Update everything” checks all
detected sources and presents a review before installation. Steam games and other
applications with their own updater keep their own workflow.

The desktop does not build AUR or other locally built packages. See
[AUR and locally built packages](#aur-and-locally-built-packages).

## Using the page

The page is written for somebody who wants to press Update and see a bar.
Under every row is the state of what it would press — “4 updates”, “Up to
date”, “Could not check” — and under **Update everything** the whole
machine's: “10 updates available”.

1. Press **Update everything**, or one source. The panel checks and then says
   what it found: a count, one line a source, and **Update now** — or
   “Everything is up to date” and a way out. “A restart may be needed
   afterwards” appears when the system or a device's firmware is among what
   would be installed, and a blocking condition appears as a plain sentence
   (“Storage is nearly full”, “Running on battery”), two at most, with the
   mount paths and megabytes behind **Full output**.
2. **Update now** authorizes updating the sources listed, and it is the
   only confirmation. Every tool runs unattended — `pacman --noconfirm`,
   `apt-get -y`, `dnf -y`, `zypper --non-interactive`, `flatpak update -y`,
   `fwupdmgr -y` and so on — so a machine with four sources does not ask
   "Proceed?" four more times. Native tools still re-resolve their
   transactions; the review is advisory, not a frozen list of package
   versions. What is waiting is listed in full in **Full output**, one row a
   package with the version change beside it, ahead of anything the tools say.
3. The shell's polkit panel asks once to authorize the job's privileged
   operations: system updates, system/named Flatpaks, Snaps and eligible firmware.
   It takes the progress panel's place and restores it when answered. The panel
   is titled **Install updates** and asks for your password to install the
   selected updates; it never shows a command line. The helper retains a
   restricted job authorization, **never a cached password**. It releases that
   authorization before reporting completion, including failed jobs.
4. During installation the panel shows the source, progress and power protection.
   Routine native output stays in **Full output**. Progress combines completed
   sources with the active tool's installation counters; it is not an estimate of
   time remaining. A native `[Y/n]` question becomes **Yes** and **No**; an
   recognizable password/passphrase/PIN prompt with echo disabled opens a masked
   response field. Echo disabled during progress alone does not request a
   password. Anything else a tool asks
   is answered with **Respond**, on the panel when the panel recognised a
   question and in **Full output** whether it did or not. Normal answers may be
   echoed by the native tool. Full-screen editors and terminal interfaces remain
   outside this integration's capabilities.
5. **Run in background** leaves the job running. Open Updates again
   to reconnect. A running installation cannot be cancelled by closing the
   panel. Read-only checks can be cancelled after the current bounded query
   finishes.
6. The finish is a sentence and a word a source — Finished, Staged for the
   next boot, Deferred, Failed — with **Restart now** first when a restart was
   staged, and the tool's reason in **Full output** when something failed.
   Excluded BIOS/UEFI devices do not make an otherwise successful job read as
   partly failed; only a provider that failed does. On Arch-based systems,
   configured ignored packages also remain skipped without turning a successful
   pacman run into a failure. The post-update query is retained in Full output;
   any remaining update that is not marked ignored still requires attention. A
   log that could not be written, or authorization that could not be cleaned
   up, is a warning beside the outcome and never the outcome itself. Successful
   command exit is not proof that every update was applied; check again to
   confirm.

   A finished background job also reaches the notification centre, with
   **View output** — or **Review restart** when one is owed — so a job left
   running while Settings was closed is not lost. Recognized Yes/No and masked
   password prompts also notify background users. Answering a prompt removes its
   obsolete notification; later questions can notify again. Looking at Recent
   updates, Preferences, or another attempt's output does not silence the current
   job's completion. Do Not Disturb still suppresses toasts while keeping the
   notification-centre entry.

7. **Recent updates** keeps the last three attempts, including ones that failed
   or were interrupted, each with its date, scope and outcome. Selecting one
   opens that job's output read-only, even while another job is running.

A count is a count of things: each provider's preview is read in its own
shape — `checkupdates`' `name old -> new`, APT's `Inst` lines, DNF's
`name.arch version repo` rows, zypper's table, flatpak's refs — and the
talk around a list is not part of it. A provider whose preview is a
deployment's status rather than a list (rpm-ostree, bootc, NixOS, Guix) is
“Ready to update” and not counted. An APT machine that listed nothing from a
package database it could not refresh without root is “None since the last
refresh”, not up to date, and is still offered the update that refreshes.

Daily checks run while Settings is open and can be disabled in Update
preferences. They do not install, download a transaction, change native schedules
or restart the computer. Repository metadata may be downloaded by checks.

No source waits on another. Each is a transaction of its own with its own
confirmation and was chosen in the review on its own, so a system update that
failed or needs attention is no reason to skip the Flatpaks or the firmware.

**Full output** is the transcript in a terminal frame: eighty columns of a
fixed-width face, the width of the terminal the tools ran on, with everything
they said. It opens directly from the review, from progress, from a result and
from Recent updates — there is no intermediate view and nothing to page.

The transcript begins with the check's own findings: a heading a source, every
package with its version change, every excluded device with its reason, and any
notices. Then the tools' output, running on from one source to the next under a
heading for each, with what became of each source written into it.

While a job runs the frame follows the tool. It stops following as soon as it is
scrolled by hand and follows again when scrolled back to the end, with **Latest
output** to return in one press. Left and Right scroll it from a controller,
Page Up and Page Down from a keyboard, and End returns to the live end; a mouse
wheel moves the text. Home is not used — Home is the guide button, which this
shell never takes away.

Each attempt is written to its own file as it happens, independently of the
status snapshots, and served back in bounded chunks rather than resent whole on
every poll. The last three ended attempts are kept — including failed,
interrupted and cancelled ones, which ordinary checks never evict — plus the
one running now. A transcript is capped at 16 MiB, and a capped one says
**Output limit reached** rather than quietly calling a tail the full output.
Attempts recorded by an older helper are kept as summaries, marked “Full output
unavailable for this older update”; that output was discarded at the time and
cannot be recovered.

Arbitrary native output can itself contain sensitive data, so there is no
promise of universal redaction; scripts must not print credentials. Authentication
buffers and response submissions are never recorded.

### AUR and locally built packages

The desktop does not check, build or install AUR packages, and there is no
**Update AUR** row. AUR helpers build with a user's own sudo authentication and
run user-supplied build scripts, which is not work this job's single
authorization can honestly cover. Requests naming AUR are refused, including
ones carried over from an older review or sent directly to the coordinator.

AUR and other locally built packages are maintained manually, with your helper
of choice, outside Settings. Nothing is uninstalled and no installed foreign
package is altered. A foreign package is not necessarily an AUR package and none
is labelled as one. An `aur_helper` key left in the administrator policy is
accepted and ignored, so that one stale key does not
stop the system updating.

## Firmware policy

**fwupd is a required desktop runtime dependency.** The package also requires
polkit. The NixOS module enables `services.fwupd` and polkit.

Routine firmware and bulk updates exclude BIOS/UEFI, embedded controllers,
management engines and unclassified device types. Metadata indicating platform
firmware overrides a friendly device name. Currently eligible protocols are
NVMe, ATA (`org.t13.ata`), Logitech Unifying/HID++, and ColorHug. This intentionally leaves many
otherwise supported fwupd devices for manual maintenance.

Device and release metadata are read again before each installation. Every
firmware command names one validated device ID and restricts its protocol. There
is no unfiltered firmware update, forced install, downgrade, remote enabling,
report upload or automatic restart. Native fwupd safety and authorization checks
still apply. Exclusions remain visible even if other updates succeed.

Checks refresh signed metadata from already configured fwupd remotes, without
forcing a download or offering to enable another remote. If no metadata is
fetched, results retain the cached label. A failed refresh is reported and blocks
installation from that review. Administrators choose their firmware remotes using
the distribution's native workflow. Device updates may require a power cycle;
consult fwupd's output. Hardware validation and additional protocol coverage
remain release work.

## Provider coverage

This is an initial implementation using native command-line bridges. Detection
and fake-tool tests do **not** establish distribution or hardware certification.
No real package upgrades or firmware writes were used for development validation.

| Host/source | Check | Installation path | Current validation |
| --- | --- | --- | --- |
| Arch and derivatives | `checkupdates`, isolated package database | Full `pacman -Syu` | Host detection and recipe tests; native install needs a VM |
| Debian/Ubuntu and derivatives | Cached APT simulation | Refresh then unattended `apt-get -y dist-upgrade`, conffiles kept | Recipe tests; VM pending |
| Fedora/RHEL family | DNF/DNF5 metadata refresh and update query | DNF live upgrade; DNF5 offline staging | Recipe tests; VM pending |
| openSUSE Leap/Tumbleweed/Slowroll | Cached Zypper list | Leap patch/update; rolling `dup` | Detection/recipe tests; VM pending |
| rpm-ostree / bootc | Deployment status | Stage a new deployment | Detection/recipe tests; VM pending |
| MicroOS/Aeon/Kalpa/SLE Micro | Cached Zypper list | Non-interactive transactional-update using configured method | Detection/recipe tests; VM pending |
| Alpine / Void / Gentoo | Native cached/pretend query | APK / XBPS / Portage | Recipe tests; VM pending |
| Solus / Mageia | Native list query | eopkg / urpmi | Implemented; VM pending |
| NixOS / Guix System | Configured ownership and source | Build and stage NixOS; pull, build and reconfigure Guix | Explicit policy required; VM pending |
| Flatpak | Remote updates for installed scopes | Each user/system/named installation separately | Local scope discovery and scope tests; install pending |
| Snap | `snap refresh --list` | Native `snap refresh` | Implemented; daemon tests pending |
| Independent Nix/Guix profiles | Profile inventory | Nix profile upgrade; Guix pull and package upgrade | Explicit policy required; profile tests pending |
| Slackware / unknown hosts | Manual maintenance notice | Native maintenance outside Settings | Automatic installation disabled |
| AppImages and application-owned content | No universal updater | Application/vendor workflow | No automatic adapter |
| fwupd | Configured remote refresh, JSON inventory and releases | Validated devices only | Fake-tool PTY/recovery/exclusion tests; hardware pending |

Host ownership requires distribution and package database evidence. Known image
and declarative deployments take precedence over installed mutable tools. An
unrecognized read-only root or failed bootc ownership query blocks automatic
system installation. Installing an extra package-manager executable cannot turn
it into the system owner.

Missing managers, unavailable services, unsupported command options, repository
errors and authorization failures are reported; they are not “up to date”. Native
solvers, locks, signatures and repository policy remain in force. Each tool's
ordinary confirmation is answered by its own unattended flag — Update now was
the confirmation — and nothing more: no `--force`, no unsigned or downgraded
package, no erased dependency, no reboot, and no distribution-release upgrade
action.

## Administrator policy

Optional `/etc/linexinbar/updates.json` must be root-owned and protected against
replacement, including its parent directories and referenced configuration paths.
Unknown keys are rejected. A changed policy invalidates an already reviewed job.
This file selects provider identities and native configuration inputs; custom
commands belong in an administrator-installed manifest described below.

For NixOS flakes:

```json
{
  "nixos": {
    "kind": "flake",
    "directory": "/etc/nixos",
    "configuration": "desktop"
  }
}
```

The flake directory, `flake.nix` and `flake.lock` must already exist. Continue
updates that lock file, builds the configured host and stages its boot generation.
A flake in the read-only Nix store cannot have its lock file updated. For an
administrator-managed channel system use `"nixos": {"kind": "channels"}`.
Channel mode requires a protected `/etc/nixos/configuration.nix` and refuses
a default `/etc/nixos/flake.nix`, avoiding an implicit change of configuration source.
The NixOS module exposes `programs.linexinbar.updatesPolicy` with this JSON shape.

Other independently applicable keys:

```json
{
  "guix_system": "/etc/config.scm",
  "independent_nix_profile": false,
  "independent_guix_profile": false
}
```

`aur_helper` is accepted and ignored for compatibility with older policies; the
desktop no longer runs AUR helpers at all.

Set profile options to true only for profiles intentionally maintained with
imperative package upgrades. Home Manager and manifest-owned profiles should be
maintained through their configuration; recognized Home Manager entries are
excluded. Nix pinned references retain their inputs. Guix uses the newly pulled
Guix executable for subsequent operations. Guix System uses root's channels and
an administrator-selected system configuration, and reconfigures live.

## Custom system update providers

Distributors can ship their own scripts for **Update the system**, including
immutable systems that stage a new image or deployment for the next boot.
Selecting a custom provider replaces the built-in System backend completely:
**Update everything** runs that provider for System, with no preceding pacman,
DNF or other native system upgrade. Flatpak, Snap, firmware and configured
independent profiles keep their separate providers.

A distribution ships three files, plus its native updater and any script
interpreter it requires:

| File in the installed image | Purpose | Typical mode |
| --- | --- | --- |
| `/usr/share/linexinbar/updates.json` | Select the provider | `0644` |
| `/usr/share/linexinbar/update-providers.d/my-distro.json` | Declare its commands | `0644` |
| `/usr/libexec/my-distro-update` | Implement the check and apply operations | `0755` |

Install these as root-owned files in the distribution's package/image build.
They and their parent directories must not be group- or world-writable, in both
`root` and `user` execution modes. Do not point a manifest at a script in an
end user's home directory. No per-user Settings configuration is necessary.

### 1. Select the System owner

Ship this as `/usr/share/linexinbar/updates.json`:

```json
{
  "system": {
    "kind": "custom",
    "id": "my-distro"
  }
}
```

The first existing policy file wins, in this order:

1. `/etc/linexinbar/updates.json` — administrator override.
2. `/usr/share/linexinbar/updates.json` — distribution default.
3. `/run/current-system/sw/share/linexinbar/updates.json` — Nix system profile.

Files are not merged. An administrator override must contain every policy option
that should apply. Only an absent entry permits fallback; a broken symlink,
unreadable override or invalid policy is an error. A missing, changed or failed
custom provider never enables native updates in its place.

The other selections are `{"system":{"kind":"native"}}` for built-in
ownership detection and `{"system":{"kind":"disabled"}}` to omit System
updates. Unused native NixOS/Guix configuration does not block a custom or
disabled System owner.

### 2. Declare the commands

Ship `/usr/share/linexinbar/update-providers.d/my-distro.json`:

```json
{
  "version": 1,
  "id": "my-distro",
  "name": "My Distribution",
  "check": {
    "executable": "/usr/libexec/my-distro-update",
    "arguments": ["check"]
  },
  "apply": {
    "executable": "/usr/libexec/my-distro-update",
    "arguments": ["apply"]
  },
  "run_as": "root"
}
```

Manifests are selected by filename from `/etc/linexinbar/update-providers.d/`,
then `/usr/share/linexinbar/update-providers.d/`, then
`/run/current-system/sw/share/linexinbar/update-providers.d/`, using the same
first-existing, no-merge rule.

`version` must be `1`. The `id` must match the filename and contain 1–80 ASCII
letters, digits, underscores or hyphens. `name` is a nonempty display name of at
most 100 UTF-8 bytes, without control characters. Unknown fields are rejected.
Executables must be absolute paths. `arguments` is a fixed array of individual
arguments, without shell expansion; it may be omitted for an empty array. No IPC
request can add arguments or provide executable paths.

`run_as` controls **apply**, and accepts `root` or `user`. The check always runs
as the desktop user. Use `root` for system deployment work that needs elevated
privileges; do not add `sudo` or `pkexec` inside the script. The existing job
worker authorizes the selected root operation, reusing that job's authorization
when System is part of Update everything. The password is never passed to the
script or cached. A `user` provider still needs mandatory power protection.

The manifest and both declared executable files are fingerprinted at review
and checked again before execution. Changes invalidate the review. This is not
a recursive audit of a script's interpreter, imported code or subprocesses:
distributors must also keep those dependencies administrator-controlled.

### 3. Implement `check`

Check is unprivileged, read-only and bounded by the coordinator's 90-second query
timeout. It must not stage/install an update or request administrator access.
Print exactly one JSON object to stdout and exit `0`; put diagnostics on stderr.
For example, an available immutable deployment can be represented as one item:

```json
{
  "version": 1,
  "availability": "available",
  "summary": "A new system image is available",
  "items": [
    { "name": "System image", "detail": "42 → 43" }
  ]
}
```

| `availability` | Meaning |
| --- | --- |
| `available` | There is work to apply; `items` can describe packages or an image. |
| `current` | Nothing needs updating; `items` must be empty and apply is skipped. |
| `unknown` | The check succeeded but cannot determine availability; it does not mean up to date. Apply can still be offered. |

`items` may be omitted and defaults to an empty array. An available/unknown
provider with no items can run without a package count. Return nonzero for an
actual query failure; do not report `current` or `unknown` to hide one.

The entire JSON response is limited to 64 KiB, with at most 10000 items and a
`summary` of at most **1000 UTF-8 bytes**. Keep item descriptions short enough to
fit the response limit. Unknown fields and unsupported versions are rejected.

### 4. Implement `apply`

Apply runs in the foreground with the configured privileges. Its stdout and
stderr go to **Full output**. Wait for the native transaction to finish, then
write exactly one result JSON object to the descriptor named by
`LXB_UPDATE_RESULT_FD`. It is currently descriptor `3`; use the environment
variable rather than treating stdout as the result channel.

```json
{
  "version": 1,
  "outcome": "staged",
  "summary": "New system image staged",
  "restart": true
}
```

| `outcome` | Meaning |
| --- | --- |
| `applied` | The update operation completed. |
| `staged` | A deployment is prepared for the next boot; `restart` must be `true`. |
| `no-changes` | Nothing needed changing by the time apply ran. |
| `needs-attention` | Work needs intervention; the coordinator records an unsuccessful source with the supplied summary. |

Exit `0` after writing a valid result. Exit nonzero on a process/transaction
failure and explain it on stderr. Exit `0` alone, missing JSON, malformed JSON,
an unsupported version or a result over 64 KiB is not success. The summary is
limited to 1000 UTF-8 bytes and must not contain control characters. `restart`
is optional and defaults to `false`.

`restart: true` requests the desktop's protected restart action; it does not
reboot immediately. The script must never reboot itself, override inhibitors,
remove native locks, or start detached update writers. All subprocesses that
write the system must finish before apply returns. Descendants must not keep the
result descriptor open after the script exits. A quiet apply operation has no
installation timeout; the coordinator does not kill a writer for being silent.

### Script example for an immutable image

The following is an adapter template for `/usr/libexec/my-distro-update`, not a
ready-made updater. Replace `/usr/libexec/my-distro-image` with the distribution's
own backend and adapt its calls. This example assumes:

- `has-update` is read-only and returns `0` for available, `1` for current, and
  another nonzero status for failure.
- `stage` runs the complete native transaction in the foreground and returns `0`
  only when a new bootable deployment was successfully staged.

Do not map an arbitrary updater's exit codes to those meanings without checking
its contract. If your backend can finish with no changes, adapt apply to return
`no-changes` instead of claiming a staged deployment.

```bash
#!/bin/bash
set -eu

case "${1:-}" in
  check)
    if /usr/libexec/my-distro-image has-update >/dev/null; then
      printf '%s\n' '{"version":1,"availability":"available","summary":"A new system image is available","items":[{"name":"System image","detail":"New deployment"}]}'
    else
      status=$?
      if [ "$status" -eq 1 ]; then
        printf '%s\n' '{"version":1,"availability":"current","summary":"System image is current","items":[]}'
      else
        printf 'Could not check for a new system image (exit %s).\n' "$status" >&2
        exit "$status"
      fi
    fi
    ;;
  apply)
    # Fail before staging if invoked without the coordinator's result channel.
    : "${LXB_UPDATE_RESULT_FD:?Apply must run through lxb-updates}"
    case "$LXB_UPDATE_RESULT_FD" in
      *[!0-9]*) printf 'Invalid result descriptor.\n' >&2; exit 2 ;;
    esac
    : >&"$LXB_UPDATE_RESULT_FD"
    /usr/libexec/my-distro-image stage
    printf '%s\n' '{"version":1,"outcome":"staged","summary":"New system image staged","restart":true}' \
      >&"$LXB_UPDATE_RESULT_FD"
    ;;
  *)
    printf 'Usage: %s check|apply\n' "$0" >&2
    exit 2
    ;;
esac
```

Both commands start in `/` with a cleared environment and a fixed system `PATH`.
Check receives `LC_ALL=C`; apply also receives terminal/pager settings and
`LXB_UPDATE_RESULT_FD`. Do not depend on the user's shell initialization, HOME,
credentials in environment variables, or working directory. Use absolute paths
for distro-specific helpers. For dynamic JSON text, use a JSON encoder rather
than interpolating unescaped package names or error messages into strings.

On NixOS, package the manifest and script into the system profile, use actual
store paths for executables/interpreters, and select the provider through
`programs.linexinbar.updatesPolicy.system = { kind = "custom"; id = "my-distro"; };`.
The module's policy option selects the provider; it does not install the provider
script or its dependencies for you. Ensure both the desktop and coordinator are
built from the matching implementation when distributing this interface.

### Integration and validation

Custom operations use the existing job serialization, power inhibitor,
authorization, retained transcripts and completion notifications. Closing the
window leaves the operation running. **Recent updates** exposes the last three
attempts. The native backend remains responsible for transaction integrity,
image verification and rollback; the coordinator cannot make arbitrary scripts
safe merely by holding an inhibitor.

Test adapters in a disposable VM with the installed helper and polkit action:
verify available/current/query-failure cases, cancellation of authorization,
staging success/failure, invalid results, a changed manifest after review,
background completion, and restart into the staged deployment. Confirm that
native system upgrades do not run alongside the custom provider. Local fake-tool
tests do not establish that a distributor's real transaction is safe.

The built-in firmware provider still requires fwupd and excludes BIOS/UEFI.
Keep firmware work outside the custom System script; this interface does not
extend firmware eligibility.

## Coordination and recovery

`lxb-updates` runs unprivileged. Its singleton lock, private status journal and
the last three job summaries and transcripts live under `$XDG_STATE_HOME/lxb/updates`
(default `~/.local/state/lxb/updates`). Its control socket is
`$XDG_RUNTIME_DIR/lxb-updates-<id>.sock`, where `<id>` names that state
directory — a Unix socket path is limited to 107 bytes, which a state directory
under a long home would exceed, and the runtime directory is private to the
account and cleared at logout. It falls back beside the state when there is no
private runtime directory. Only same-account peers are answered. The desktop and
helper must be from the same build. Protocol 3 checks the request version before
any operation; older wire requests cannot start an installation. Old journals
retain interruption and staged-restart evidence, but old reviews are invalidated.

### Job authorization

An installed, root-owned `lxb-updates` is required for privileged work. One
`pkexec --disable-internal-agent … privileged-job` starts a worker with a finite,
reviewed grant over an inherited anonymous Unix socket. Subsequent requests name
only a source and step index. The worker derives fixed commands itself, verifies
protected executable paths and administrator policy, and reclassifies each
firmware device before installation. It rejects repeated steps, unselected
operations and user Flatpak installations. No arbitrary command, environment,
user profile can be submitted to it, and AUR is refused outright.

The coordinator and root worker disable core dumps and same-account ptrace/proc
FD access. The grant has no filesystem token and no session-wide polkit KEEP
permission. Normal completion drops the grant and power guard before acknowledging
closure; connection loss starts no further privileged operations. An active native
writer keeps its PTY and protection while it finishes, even if its client goes
away. A tool waiting for input after that disconnect can require manual recovery.
This is authorization reuse, not password replay: passwords are not placed in
arguments, environment variables, job state, logs or cache files. Authentication
still passes through the shell's normal polkit agent and the system authenticator.

The action is `org.linexinbar.updates.install`, shipped with the package and
bound to the installed helper's path and its `privileged-job` first argument, so
polkit shows the updates title and message rather than a command line. The shell
gives that title to this exact action and to nothing else: it never dresses up an
arbitrary `pkexec` request by matching command-line text, and other applications'
authentication stays generic. A missing policy file fails packaging validation
rather than being papered over with a string replacement in the shell. The
binding and message are [pkexec action annotations](https://polkit.pages.freedesktop.org/polkit/pkexec.1.html).
Administrator authentication is required, without KEEP caching, so a second job
asks again.

### Power and session protection

Before any installation, a **mandatory** logind/elogind block inhibitor covers
shutdown, sleep, idle sleep, power/suspend/hibernate keys and the lid switch.
Protection failure, or an already-started shutdown/sleep transition, stops the job
before mutation. Unsupported service-manager/logind configurations fail closed.
The coordinator first attempts unprivileged inhibition. A user service outside
logind session attribution may need the privileged worker to obtain protection.
The worker transfers a duplicate inhibitor descriptor over its private socket
using SCM_RIGHTS; it retains its own descriptor too. The coordinator refuses to
continue without receiving that descriptor. Losing either process leaves the
survivor holding protection for its native operations. The duplicate conveys no
permission to run commands and is close-on-exec. Both descriptors are released
when their native operations and authorization cleanup finish.

The shell's shutdown, restart, suspend and logout actions take a live lock that
conflicts with installation. This closes the race between status polling and
pressing a power button. The updater's staged restart uses the same guard.
A root-owned `/run/linexinbar/updates.lock` also serializes privileged jobs across
accounts and guards orphaned root writers. These advisory locks supplement native
package-manager locks; they do not remove locks or interrupt external transactions.

When available, the coordinator runs in a transient systemd user service outside
the compositor's session scope; otherwise it starts detached. Closing the Updates
panel is safe. Shell logout is blocked during installation, but external forced
session termination, an administrator overriding inhibitors, hardware power loss,
or a forced power-button reset cannot be prevented. The integration does not
enable lingering or alter host logout policy. Validate supervision on each target
service manager before promising survival of external session teardown.

The coordinator is not kept for life. Fifteen minutes after the last request,
with no job running and no restart staged, it quits and removes its socket; the
desktop starts another on the next request, from the journal on disk. A running
transaction or a staged restart keeps it alive, since only a live coordinator
can carry those out. `LXB_UPDATES_IDLE_SECONDS` shortens the wait for tests.

Basic host battery/free-space checks run before installation. These checks
supplement the native solver's exact storage and hardware requirements.

Checkpoint failure prevents subsequent operations while allowing an already
running native transaction to finish. Interrupted jobs are reported and never
replayed automatically. Recovery still requires reviewing native locks/history.
A staged update exposes a separate restart action. Another system deployment is
blocked until restart; a changed boot ID asks for verification instead of claiming
success. Automatic rollback and provider-specific post-boot reconciliation are
not implemented yet.

Each attempt's full transcript is written to a private, same-account file under
the state directory as it happens, with no caller-selected paths, atomic
metadata and crash-safe rotation. A disk or log error never kills a package
writer: the recording failure is reported, further work is stopped according to
checkpoint policy, and the installation's outcome stays distinct from the log's
— a job whose providers all succeeded and whose log could not be written is a
success with a warning, not a partial failure.

Native password responses are sent outside the serializable request model;
bounded response buffers are erased even after partial-read failures, without
buffered read-ahead; the process and kernel are not protected against swapping.
Native output and ordinary echoed answers can appear in the local transcript.

## Verification and release work

Run `cargo test -p lxb-updates --lib` and the fake-tool integration test
`cargo test -p lxb-updates --test coordinator` as a non-root account with local
Unix sockets and PTYs available. Desktop tests selected by
`cargo test -p lxb-desktop update` cover action routing, the review's layout,
the terminal frame and prompt geometry. `packaging/check.sh --no-build` checks staged
payloads using existing release binaries. It does not validate those binaries
against each target distribution.

To look at the page itself, `scripts/updates-shot.sh OUT_DIR ACTIONS` drives a
nested session against the fake tools in `scripts/updates-fixture/bin` — a
system with updates, two Flatpak scopes and three fwupd devices
of which one is eligible — and photographs it. A number in
`OUT_DIR/home/fixture-wait` is how long each fake holds its question, so the
Yes and No can be photographed. A root step cannot be faked by
design: `pkexec` runs only a root-owned executable, so the fake `pacman` is
refused with "not protected against replacement" and the failure path is what
is photographed for the system. Nothing real is touched; scratch state
and test logs stay under the output directory. The harness starts the integration
test executable with an injected fake power guard and stops that process afterwards.
The shipped helper has no protection-bypass flag. These pictures validate layout,
not real power inhibition or privileged authorization.

Before a distribution release, run isolated VM tests for native prompts, declined
transactions, package conflicts and locks, power loss/reconnect, staged boot
success/failure and supported manager versions. Add structured native transaction
plans and post-boot reconciliation before claiming exact preview/apply parity.
Expand firmware eligibility only with representative metadata and hardware tests.
Current implementation status is tracked in the repository's `UPDATES-TO-DO.MD`.

## Native references

- [Arch system maintenance](https://wiki.archlinux.org/title/System_maintenance)
- [DNF5 offline transactions](https://dnf5.readthedocs.io/en/stable/commands/offline.8.html)
- [Flatpak installation API](https://docs.flatpak.org/en/latest/libflatpak-api-reference.html)
- [transactional-update](https://kubic.opensuse.org/documentation/man-pages/transactional-update.8.html)
- [fwupd ATA protocol and activation](https://fwupd.github.io/libfwupdplugin/ata-README.html)
- [GNU Guix manual](https://guix.gnu.org/manual/en/guix.pdf)

- [polkit authentication and temporary authorizations](https://polkit.pages.freedesktop.org/polkit/polkit.8.html)
- [logind manager and inhibitors](https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html)
