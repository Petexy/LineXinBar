# What was changed, and why

The directory and the package are named `lxb-smithay` so that nothing — a
lock file, `cargo tree`, a vendored source archive — can take this for the
published crate. The library it builds is still `smithay`.

Smithay 0.7.0 verbatim, apart from one addition: an atomic page flip that does
not wait for the vertical retrace.

The kernel has accepted `DRM_MODE_PAGE_FLIP_ASYNC` on atomic commits for years,
and the `drm` crate exposes it as `AtomicCommitFlags::PAGE_FLIP_ASYNC`, but
smithay's atomic surface never sets it and offers no way to ask for it. A
compositor built on `DrmOutput` therefore cannot tear at all, whatever a client
requests — which makes `wp_tearing_control_v1` unimplementable. Upstream has no
such support on master either, checked at 347b2b3.

Three files:

* `src/backend/drm/surface/atomic.rs` — `AtomicDrmSurface` gains a `tearing`
  flag, read when building the page flip's commit flags. A refused immediate
  flip is retried at the retrace rather than dropped, because tearing is a hint
  and a driver may refuse any given frame: amdgpu does whenever the commit
  changes more than the primary plane's address.
* `src/backend/drm/surface/mod.rs` — `DrmSurface::set_tearing`, which is what
  the compositor calls. A no-op on the legacy surface.
* `Cargo.toml` — the `[[example]]` and `[[bench]]` targets are dropped, because
  their sources are not vendored here, and a `[lints.rust]` table allows the
  warnings this release emits on its own. Cargo caps lints on a crate it
  fetched from the registry and does not cap them on one it builds from a path,
  so vendoring made eight upstream warnings print on every build of this
  workspace. They are upstream's to fix; allowing them keeps a warning from
  this project's own code recognisable as one.

Nothing else is touched, so this tracks 0.7.0's behaviour exactly everywhere the
flag is not set — which is every frame that has not asked to tear.

Take care where `set_tearing` is inserted: putting it directly above
`page_flip` rather than above `page_flip`'s doc comment moves that comment and
its `#[profiling::function]` onto the new method, which silently drops
`page_flip` out of the profiler and out of the documentation.

To rebase onto a new smithay: take the release verbatim, drop the example and
bench targets, and re-apply the three edits above. They are marked in the source
with `LineXinBar patch`.
