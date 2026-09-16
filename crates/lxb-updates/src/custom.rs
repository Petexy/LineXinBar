//! Administrator-installed system providers. IPC selects an identity, never a command.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, path::Path};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Owner {
    Native,
    Disabled,
    Custom { id: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub executable: String,
    #[serde(default)]
    pub arguments: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunAs {
    User,
    Root,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    pub id: String,
    pub name: String,
    pub check: Command,
    pub apply: Command,
    pub run_as: RunAs,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reviewed {
    pub manifest: Manifest,
    pub fingerprint: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Availability {
    Available,
    Current,
    Unknown,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub version: u32,
    pub availability: Availability,
    #[serde(default)]
    pub items: Vec<crate::Item>,
    pub summary: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Applied,
    Staged,
    NoChanges,
    NeedsAttention,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Applied {
    pub version: u32,
    pub outcome: Outcome,
    pub summary: String,
    #[serde(default)]
    pub restart: bool,
}
impl Applied {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 65536 {
            bail!("Provider result exceeded its limit");
        }
        let result: Self = serde_json::from_slice(bytes)
            .context("The custom provider did not return a valid result on LXB_UPDATE_RESULT_FD")?;
        if result.version != 1
            || result.summary.len() > 1000
            || result.summary.chars().any(char::is_control)
            || (result.outcome == Outcome::Staged && !result.restart)
        {
            bail!("Invalid custom provider outcome");
        }
        Ok(result)
    }
}
pub fn identity(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
}
impl Manifest {
    fn validate(&self) -> Result<()> {
        if self.version != 1
            || !identity(&self.id)
            || self.name.is_empty()
            || self.name.len() > 100
            || self.name.chars().any(char::is_control)
        {
            bail!("Invalid custom provider identity/version");
        }
        for command in [&self.check, &self.apply] {
            if !Path::new(&command.executable).is_absolute()
                || command.arguments.len() > 64
                || command
                    .arguments
                    .iter()
                    .any(|s| s.len() > 4096 || s.contains('\0'))
            {
                bail!("Invalid provider command");
            }
        }
        Ok(())
    }
}
fn digest_file(path: &Path, digest: &mut Sha256) -> Result<()> {
    crate::policy::protected(path)?;
    let mut file = File::open(path)?;
    if file.metadata()?.len() > 128 * 1024 * 1024 {
        bail!("Provider executable exceeds verification limit");
    }
    let mut bytes = [0u8; 32768];
    loop {
        let n = file.read(&mut bytes)?;
        if n == 0 {
            break;
        }
        digest.update(&bytes[..n]);
    }
    Ok(())
}
pub fn load(id: &str) -> Result<Reviewed> {
    if !identity(id) {
        bail!("Invalid provider ID");
    }
    let admin = format!("/etc/linexinbar/update-providers.d/{id}.json");
    let vendor = format!("/usr/share/linexinbar/update-providers.d/{id}.json");
    let nix = format!("/run/current-system/sw/share/linexinbar/update-providers.d/{id}.json");
    let path =
        crate::policy::first_existing([Path::new(&admin), Path::new(&vendor), Path::new(&nix)])?
            .context(
                "The configured system update provider is missing; native fallback is disabled",
            )?;
    crate::policy::protected(path)?;
    let mut bytes = vec![];
    File::open(path)?.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        bail!("Provider manifest too large");
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    manifest.validate()?;
    if manifest.id != id {
        bail!("Provider identity does not match its filename");
    }
    let mut digest = Sha256::new();
    digest.update(&bytes);
    for command in [&manifest.check, &manifest.apply] {
        digest_file(Path::new(&command.executable), &mut digest)?;
    }
    Ok(Reviewed {
        manifest,
        fingerprint: format!("{:x}", digest.finalize()),
    })
}
pub fn selected() -> Result<Option<Reviewed>> {
    match crate::policy::read()?.system {
        Some(Owner::Custom { id }) => Ok(Some(load(&id)?)),
        _ => Ok(None),
    }
}
impl Reviewed {
    pub fn verify(&self) -> Result<()> {
        if selected()?.as_ref() != Some(self) {
            bail!("The custom update provider changed. Check again before updating");
        }
        Ok(())
    }
    pub fn step(&self) -> Result<crate::process::Step> {
        self.verify()?;
        let command = &self.manifest.apply;
        let args: Vec<_> = command.arguments.iter().map(String::as_str).collect();
        let mut step = crate::process::Step::new(
            &command.executable,
            &args,
            self.manifest.run_as == RunAs::Root,
        );
        step.custom = Some(self.clone());
        Ok(step)
    }
    pub fn check(&self) -> Result<Check> {
        self.verify()?;
        let command = &self.manifest.check;
        let mut cmd = std::process::Command::new(&command.executable);
        cmd.args(&command.arguments)
            .env_clear()
            .env("PATH", crate::process::SYSTEM_PATH)
            .env("LC_ALL", "C")
            .current_dir("/");
        let output = crate::process::probe_command(cmd, "custom provider check", &[0])?;
        if output.text.len() > 65536 {
            bail!("Provider check exceeded its limit");
        }
        let result: Check = serde_json::from_str(&output.text)?;
        if result.version != 1
            || result.summary.len() > 1000
            || result.items.len() > 10000
            || (result.availability == Availability::Current && !result.items.is_empty())
        {
            bail!("Invalid provider check result");
        }
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_and_results_fail_closed() {
        assert!(!identity("../../script"));
        assert!(!identity("shell;reboot"));
        assert!(load("missing-fixture-provider").is_err());
        assert!(Applied::parse(br#"{"version":1,"outcome":"staged","summary":"ready"}"#).is_err());
        assert!(Applied::parse(
            br#"{"version":1,"outcome":"applied","summary":"done","command":"reboot"}"#
        )
        .is_err());
        assert!(Applied::parse(
            br#"{"version":1,"outcome":"staged","summary":"ready","restart":true}"#
        )
        .is_ok());
        assert!(Applied::parse(b"success").is_err());
    }
}
