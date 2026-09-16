//! Declarative systems need an administrator-selected configuration source.
//! No arbitrary hook commands are accepted. This policy only selects inputs
//! for the same fixed native update operations used by the other providers.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub system: Option<crate::custom::Owner>,
    pub nixos: Option<Nixos>,
    pub guix_system: Option<String>,
    pub aur_helper: Option<String>,
    #[serde(default)]
    pub independent_nix_profile: bool,
    #[serde(default)]
    pub independent_guix_profile: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Nixos {
    Channels,
    Flake {
        directory: String,
        configuration: String,
    },
}

pub fn protected(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        bail!("Policy paths must be absolute");
    }
    // A protected target reached through a writable symlink parent is still
    // replaceable between validation and the native tool opening the input.
    for ancestor in path.ancestors() {
        let m = std::fs::symlink_metadata(ancestor)?;
        if m.uid() != 0 || (!m.file_type().is_symlink() && m.mode() & 0o022 != 0) {
            bail!(
                "{} must be administrator-owned and protected from replacement",
                ancestor.display()
            );
        }
    }
    let canonical = path
        .canonicalize()
        .with_context(|| format!("{} is unavailable", path.display()))?;
    for path in canonical.ancestors() {
        let m = path.metadata()?;
        if m.uid() != 0 || m.mode() & 0o022 != 0 {
            bail!(
                "{} must be administrator-owned and protected from replacement",
                path.display()
            );
        }
    }
    Ok(())
}

/// Only an absent directory entry permits fallback. A dangling symlink or an
/// unreadable override is a configuration error, not permission to update the
/// host through a different provider.
pub(crate) fn first_existing<'a>(
    paths: impl IntoIterator<Item = &'a Path>,
) -> Result<Option<&'a Path>> {
    for path in paths {
        match path.symlink_metadata() {
            Ok(_) => return Ok(Some(path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // ENOENT can mean a broken parent symlink, too. Treat that
                // override as broken rather than silently choosing a vendor.
                for parent in path.ancestors().skip(1) {
                    match parent.symlink_metadata() {
                        Ok(_) => {
                            parent
                                .metadata()
                                .with_context(|| format!("Cannot resolve {}", parent.display()))?;
                            break;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => {
                            return Err(error)
                                .with_context(|| format!("Cannot inspect {}", parent.display()))
                        }
                    }
                }
            }
            Err(error) => {
                return Err(error).with_context(|| format!("Cannot inspect {}", path.display()))
            }
        }
    }
    Ok(None)
}

pub fn read() -> Result<Policy> {
    let Some(path) = first_existing([
        Path::new("/etc/linexinbar/updates.json"),
        Path::new("/usr/share/linexinbar/updates.json"),
        Path::new("/run/current-system/sw/share/linexinbar/updates.json"),
    ])?
    else {
        return Ok(Policy::default());
    };
    protected(path)?;
    let policy: Policy = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    // A replacement owns System completely. Unused native configuration must
    // not block it (or cause discovery to fall back to a mutable manager).
    if matches!(
        policy.system,
        Some(crate::custom::Owner::Custom { .. } | crate::custom::Owner::Disabled)
    ) {
        return Ok(policy);
    }
    if matches!(policy.nixos, Some(Nixos::Channels)) {
        protected(Path::new("/etc/nixos/configuration.nix"))?;
        if Path::new("/etc/nixos/flake.nix").exists() {
            bail!(
                "NixOS has a default flake. Select that flake explicitly instead of channel mode."
            );
        }
    }
    if let Some(Nixos::Flake {
        directory,
        configuration,
    }) = &policy.nixos
    {
        if !Path::new(directory).is_absolute()
            || directory.contains('#')
            || directory.chars().any(char::is_control)
            || configuration.is_empty()
            || !configuration
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            bail!("Invalid NixOS configuration identity");
        }
        protected(Path::new(directory))?;
        protected(&Path::new(directory).join("flake.nix"))?;
        protected(&Path::new(directory).join("flake.lock"))?;
    }
    if let Some(file) = &policy.guix_system {
        if !Path::new(file).is_absolute() {
            bail!("The Guix system configuration must be an absolute path");
        }
        protected(Path::new(file))?;
    }
    // Legacy aur_helper is accepted for migration, never executed.
    Ok(policy)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_override_never_falls_back_to_vendor_or_native() {
        let root = std::env::temp_dir().join(format!(
            "lxb-policy-{}-{}",
            std::process::id(),
            crate::now()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let admin = root.join("admin.json");
        let vendor = root.join("vendor.json");
        let paths = [admin.as_path(), vendor.as_path()];
        assert_eq!(first_existing(paths).unwrap(), None);
        std::fs::write(&vendor, "{}").unwrap();
        assert_eq!(first_existing(paths).unwrap(), Some(vendor.as_path()));
        std::os::unix::fs::symlink(root.join("missing.json"), &admin).unwrap();
        assert_eq!(first_existing(paths).unwrap(), Some(admin.as_path()));
        assert!(protected(&admin).is_err());
        // Even an error inspecting an override must not select the valid
        // vendor path. ENOTDIR is deterministic without changing permissions.
        let invalid = vendor.join("policy.json");
        assert!(first_existing([invalid.as_path(), vendor.as_path()]).is_err());
        let broken_parent = admin.join("policy.json");
        assert!(first_existing([broken_parent.as_path(), vendor.as_path()]).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn policies_cannot_supply_commands() {
        assert!(serde_json::from_str::<Policy>(r#"{"command":"curl example | sh"}"#).is_err());
        assert!(serde_json::from_str::<Policy>(
            r#"{"nixos":{"kind":"flake","directory":"/etc/nixos","configuration":"desktop"}}"#
        )
        .is_ok());
    }
}
