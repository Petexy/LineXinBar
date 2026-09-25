# Pre-release security review — 2026-09-22

This review found and patched two high-priority Wayland permission bypasses,
portal caller impersonation, unsafe credential scratch-file handling, and ZIP
resource-limit gaps. It also updated a vulnerable TLS dependency. This is a
source review and regression test result, not a certification that the desktop
isolates malicious applications.

## Patched findings

| Priority | Finding and impact | Change |
| --- | --- | --- |
| High | Any client connected to the Wayland display could bind screencopy and read other applications' pixels without the sharing prompt. | [screencopy.rs](../crates/lxb-compositor/src/screencopy.rs) now filters the global using the compositor's existing trusted shell/portal process identities. |
| High | Input method and virtual keyboard globals accepted all clients, permitting synthetic keyboard input and access to text input through an input method. | [text_input.rs](../crates/lxb-compositor/src/text_input.rs) restricts both globals to the session shell. Ordinary text-input clients remain supported. |
| Medium | Backend D-Bus methods trusted caller-provided application identities and session handles without checking who sent them. A local caller could impersonate an application in consent prompts or interfere with another capture session. | [caller.rs](../crates/lxb-portal/src/caller.rs) checks the bus-assigned sender against the current owner of `org.freedesktop.portal.Desktop`. All file chooser and screencast methods check it before acting. Session close is the exception described below. |
| Medium | Session close is the one backend method whose refusal is not the safe answer: it is the only path that stops a running cast, so a rejected call leaves the screen being read with nothing left to stop it. The well-known name moves between processes whenever the front desk is replaced, which is the documented repair for a stale portal. | [screencast.rs](../crates/lxb-portal/src/screencast.rs) records the unique name that opened each session and accepts close from it, falling back to the current name owner. A unique name is never reissued, so this is stricter than the name check as well as being immune to the handover window. |
| Medium, conditional on filesystem state | Steam token writes reused a predictable `steam.writing` path with create/truncate. An existing symlink could redirect the write, existing permissions were retained, and failure to protect the parent directory was ignored. | [session.rs](../crates/lxb-steam/src/session.rs) requires the session directory to resolve to one this user owns, propagates permission failures, and writes through an exclusive randomly named 0600 scratch file followed by atomic rename. Failed writes clean up their scratch file, and sign-out sweeps any a killed write left behind. The status file beside the token goes through the same write; it was a plain `fs::write` that followed a symlink at its own name. |
| Medium | ZIP extraction bounded individual deflated files but not the combined retained output. Repeated central-directory entries could exhaust memory. A forged size and prefix CRC could also cause truncated deflate output to be accepted. | [zip.rs](../crates/lxb-retroarch/src/zip.rs) checks a 512 MiB aggregate budget before extraction, applies 256 MiB entry limits to both stored and deflated files, rejects NUL names, and reads one extra byte to detect output beyond the declared size. |
| Medium | Locked `rustls` 0.23.43 accepted certain TLS handshake messages at the wrong encryption level. | [Cargo.lock](../Cargo.lock) updates `rustls` to 0.23.45 and `rustls-webpki` to 0.103.15. See [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285.html). |

The Wayland restrictions intentionally affect direct capture tools and external
input methods. Screen-sharing applications should use the portal. The existing
`--insecure-trust-program` option remains an explicit development escape hatch;
it grants broad session privileges and should not be used in a release session.

## Remaining risks and release decisions

1. **Input injection through `/dev/uinput` remains possible.**
   [The packaged udev rules](../packaging/files/70-linexinbar-input.rules) grant
   the active account access to this device. Every unsandboxed application under
   that account can use the same grant to publish a virtual keyboard. Closing
   the Wayland virtual-keyboard protocol does not close this kernel interface.
   Removing the rule affects controller interception and virtual gamepads;
   preserving those features while enforcing isolation needs an authenticated
   device broker or an application sandbox that withholds device access.

2. **Steam integration relies on a powerful local debugger.**
   [webui.rs](../crates/lxb-steam/src/webui.rs) enables Steam's Chromium debugging
   interface for integration. Removing the marker after startup does not revoke
   the interface from an already running client. Loopback access is not process
   authentication. Other processes that can reach the port can drive the Steam
   interface; its use must not be represented as isolation from local apps.
   Replacing this integration or containing access requires a separate design.

3. **Some dependency findings remain and are not suppressed.**
   `cargo-audit` reports one vulnerability and four informational warnings after
   the TLS update. Inspection did not identify a reachable vulnerable operation
   for the three code-level advisories below, but this is not a guarantee about
   future code or dependency changes.

   | Dependency | Advisory | Assessment of this tree |
   | --- | --- | --- |
   | `rsa` 0.9.10 | [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html) | No patched release in the database. The carried Steam protocol fork uses public-key encryption, not private-key operations exposed to a timing attacker. |
   | `lru` 0.16.4 via `glyphon` | [RUSTSEC-2026-0253](https://rustsec.org/advisories/RUSTSEC-2026-0253.html) | The advisory requires a key destructor that panics during `pop()`, unwinding, and subsequent cache use. Glyphon's key is a `Copy` enum with no destructor. A fixed major version requires a compatible glyphon update or a maintained fork. |
   | `cgmath` 0.18.0 via the Smithay fork | [RUSTSEC-2026-0197](https://rustsec.org/advisories/RUSTSEC-2026-0197.html), [RUSTSEC-2026-0196](https://rustsec.org/advisories/RUSTSEC-2026-0196.html) | No `swap_columns` calls were found in the compositor or carried Smithay source. The dependency is also unmaintained. |
   | `ttf-parser` 0.25.1 | [RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192.html) | Unmaintained dependency; plan an upstream-supported migration. |

## Scope and verification

Reviewed privileged update and locale entry points, polkit packaging, Wayland
privilege checks, portal interfaces, Steam credential handling, network and
archive entry points, and the workspace lockfile. Existing update protections
include fixed-operation authorization, protected executable paths, and private
runtime sockets; their regression tests were included. Native package managers,
archive tools, GPU drivers, Steam, and the bundled C code were not exhaustively
audited or fuzzed. No real system update, account sign-in, or hardware capture
was performed.

RustSec database: 1,261 advisories, commit
`eac8bd26b6593aafeb228d0f7adeb12c373e9f84`; checked with `cargo-audit` 0.22.2.
No audit findings were hidden with ignore rules.

Regression tests passed: the whole workspace, **2,496 tests**, including
compositor 256, shell 1,798, portal 3, RetroArch 88, Steam 269, and updates 31
unit tests with 14 coordinator integration tests. The existing Steam
documentation example is ignored. Added checks cover untrusted Wayland clients
and the session shell and portal keeping what they had, frontend D-Bus
ownership changes and who may close a session across one, a symlink at the old
credential scratch path and at the status file's own name, a linked session
directory, scratch files a killed write left behind, forged ZIP output size,
and aggregate ZIP limits. `cargo clippy --workspace --all-targets`, `cargo fmt
--all --check`, and `git diff --check` also passed.

Two things the first pass of these changes got wrong were found by review and
fixed before this was written down. The session directory was checked with
`symlink_metadata`, so a data directory the user had moved to another disk and
left behind as a link failed every save from then on — a sign-in that silently
stopped surviving a restart. And the Wayland checks were tested only against a
client with no peer credentials at all, which is refused whatever the rule
says; the positive case — the shell still typing, the portal still receiving
frames and still not typing — is now what the test asserts.

The tests requiring Unix sockets and an isolated D-Bus were run outside the
execution sandbox. The first sandboxed portal build failed because Bindgen's
macro fallback writes temporary files in its Cargo source directory; rebuilding
the bindings outside that sandbox resolved the failure.

Before shipping, smoke-test screen-share consent and cancellation, the on-screen
keyboard, and controller behavior in a packaged native session. Resolve or
explicitly accept the two architectural risks above before promising isolation
from untrusted games and applications.
