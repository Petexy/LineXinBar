//! The version this helper reports is `VERSION` at the root of the checkout —
//! the same single file every package definition reads.
//!
//! Cargo cannot read that file: a manifest carries a literal, and `--version`
//! compiles in `CARGO_PKG_VERSION`. So this refuses the build when the two have
//! drifted apart, as `lxb-retroarch`'s does and for the reason it gives: this
//! is a binary that can be installed *without* the shell, so the version is
//! what says which build is on the other side of the pipe when the two do not
//! agree. What they agree on is the protocol number in `report.rs`.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let Some(root) = Path::new(&manifest_dir).parent().and_then(Path::parent) else {
        eprintln!("cannot find the checkout root above {manifest_dir}");
        return ExitCode::FAILURE;
    };
    let version_file = root.join("VERSION");
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
