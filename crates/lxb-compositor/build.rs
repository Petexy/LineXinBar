//! The version this compositor reports is `VERSION` at the root of the
//! checkout — the same single file every package definition reads, so a
//! release is one edit rather than four kept in step by hand.
//!
//! Cargo cannot read that file: a manifest carries a literal, and `--version`
//! compiles in `CARGO_PKG_VERSION`. So this does the other half of the job and
//! refuses the build when the two have drifted apart. `lxb --version`
//! disagreeing with the package it was installed from is the kind of thing
//! nobody notices until a bug report; `scripts/bump-version.sh` writes both at
//! once so it cannot happen by accident.
//!
//! The compositor is also built on its own — `cargo build -p lxb-compositor`
//! is what the compositor-only package does, without the shell in the graph at
//! all — so it carries this check itself rather than trusting a sibling crate
//! to have run one.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let Some(root) = Path::new(&manifest_dir).parent().and_then(Path::parent) else {
        eprintln!("cannot find the checkout root above {manifest_dir}");
        return ExitCode::FAILURE;
    };
    let version_file = root.join("VERSION");

    // Naming the one file the answer depends on: without this cargo re-runs the
    // script whenever anything in the package changes, and with it the check
    // costs nothing on every build that is not a release.
    println!("cargo:rerun-if-changed={}", version_file.display());

    let released = match std::fs::read_to_string(&version_file) {
        Ok(text) => text.trim().to_owned(),
        Err(error) => {
            eprintln!("cannot read {}: {error}", version_file.display());
            return ExitCode::FAILURE;
        }
    };
    let manifest_version =
        std::env::var("CARGO_PKG_VERSION").expect("cargo sets CARGO_PKG_VERSION");

    if released != manifest_version {
        eprintln!(
            "{} says {released}, but this crate builds as {manifest_version}.\n\
             VERSION is the project's version; `scripts/bump-version.sh {released}` \
             writes it into Cargo.toml and the Fedora spec as well.",
            version_file.display()
        );
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}
