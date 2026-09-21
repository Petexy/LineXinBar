# What this is, and what was changed

`steam-cm-protocol` 0.4.1 from crates.io
(<https://github.com/LargeModGames/steam-cm-protocol>), MIT, carried in this
tree rather than depended on from the registry.

**The directory and the package are named `lxb-steam-cm-protocol` so that
nothing — a lock file, `cargo tree`, a vendored source archive, a packager
reading the spec — can take this for the published crate. It is not.** The
library it builds is still `steam_cm_protocol`, so `use steam_cm_protocol::`
means what it has always meant.

What this copy does that 0.4.1 does not:

- **An error packet is no longer decoded as an empty successful protobuf.** The
  published response helpers do that, and the shell cannot tell "Steam said no"
  from "Steam said nothing" — so a service error would quietly erase the last
  good library instead of leaving it standing.
- **Four messages 0.4.1 leaves out**, because it never needed them: among them
  the app ownership ticket, which is how Steam is asked whether this account may
  run a game, and the depot decryption key, without which downloaded content
  cannot be read. Plain additions to the enum and the `.proto` files — nothing
  published was changed to make room for them.
- **Game invitations, which 0.4.1 decodes as nothing at all.** Its
  `decode_incoming` answers only about text and typing, so a friend asking this
  account to join them in a game was dropped in the packet loop — and the
  invitation Steam sends *outside* the chat service, `ClientInviteToGame`
  (EMsg 7005), had no number in the enum to be recognised by. Both routes are
  decoded here into one `chat::GameInvite`; which of the two Steam actually
  uses is not something that can be settled from this side, and a client that
  read only one would receive no invitations and say nothing about it.
- **`src/achievements.rs`** and `proto/steammessages_player.steamclient.proto`:
  the read-only stats and bulk-progress calls the Trophies column is built on.

Upstream is not patched anywhere else. Anything under `src/` that is not named
above is 0.4.1 as published.
