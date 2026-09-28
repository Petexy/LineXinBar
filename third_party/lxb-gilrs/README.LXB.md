# What this is, and what was changed

`gilrs` 0.11.2 from crates.io (<https://gitlab.com/gilrs-project/gilrs>,
commit `07e286e24b046cf39e5c367daa2770b805a64692`), Apache-2.0 or MIT, carried
in this tree rather than depended on from the registry. `LICENSE-MIT` and
`LICENSE-APACHE` are upstream's, from that commit; the published crate does
not include them. `SDL_GameControllerDB/` is SDL's mapping database as the
crate ships it, under its own zlib licence.

**The directory and the package are named `lxb-gilrs` so that nothing — a lock
file, `cargo tree`, a vendored source archive, a packager reading the spec —
can take this for the published crate.** The library it builds is still
`gilrs`, so `use gilrs::` means what it has always meant.

**Nothing in it is changed.** It is carried for one reason: the fix this shell
needs is in `gilrs-core`, and a fork renamed `lxb-gilrs-core` cannot stand in
for the registry's `gilrs-core` underneath the registry's `gilrs`. So this copy
exists to depend on `../lxb-gilrs-core` — the one line in `Cargo.toml` beside
the renamed package — and on nothing else of its own. See
`../lxb-gilrs-core/README.LXB.md` for the fix itself.
