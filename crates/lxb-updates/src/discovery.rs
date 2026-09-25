//! Host detection uses deployment evidence before distribution ancestry. Merely
//! finding a package-manager binary never selects it as the host's owner.
use crate::{
    firmware,
    listing::{self, Listing},
    now,
    process::{self, Step},
    Item, Provider, Source, SourceId, System,
};
use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Default, Debug)]
pub struct Host {
    pub release: BTreeMap<String, String>,
    pub ostree: bool,
    pub bootc: bool,
    pub deployment_unknown: bool,
    pub read_only_root: bool,
    pub transactional: bool,
    pub nixos: bool,
    pub guix: bool,
    pub databases: Vec<String>,
    pub dnf5: bool,
}

pub fn os_release(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            if !key.bytes().all(|c| c.is_ascii_uppercase() || c == b'_') {
                return None;
            }
            Some((
                key.into(),
                value.trim().trim_matches(['\'', '"']).to_owned(),
            ))
        })
        .collect()
}

impl Host {
    pub fn read() -> Self {
        let release = std::fs::read_to_string("/etc/os-release")
            .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
            .unwrap_or_default();
        let release = os_release(&release);
        let ostree = Path::new("/run/ostree-booted").exists();
        let bootc_status = if ostree && process::find("bootc").is_some() {
            Some(
                process::probe("bootc", &["status", "--json"], &[0])
                    .and_then(|o| Ok(serde_json::from_str::<serde_json::Value>(&o.text)?)),
            )
        } else {
            None
        };
        let deployment_unknown = bootc_status.as_ref().is_some_and(|s| s.is_err());
        let bootc = bootc_status.and_then(Result::ok).is_some_and(|v| {
            v.pointer("/status/booted/image")
                .is_some_and(|v| !v.is_null())
        });
        let mut fs = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        let read_only_root = unsafe {
            libc::statvfs(c"/".as_ptr(), fs.as_mut_ptr()) == 0
                && fs.assume_init().f_flag & libc::ST_RDONLY != 0
        };
        let id = release.get("ID").map(String::as_str).unwrap_or("");
        let transactional = matches!(
            id,
            "opensuse-microos" | "opensuse-aeon" | "opensuse-kalpa" | "sle-micro"
        ) || release
            .get("VARIANT_ID")
            .is_some_and(|v| matches!(v.as_str(), "microos" | "aeon" | "kalpa"));
        let mut databases = vec![];
        for (manager, path) in [
            ("pacman", "/var/lib/pacman/local"),
            ("apt", "/var/lib/dpkg/status"),
            ("rpm", "/usr/lib/sysimage/rpm"),
            ("rpm", "/var/lib/rpm"),
            ("apk", "/lib/apk/db/installed"),
            ("xbps", "/var/db/xbps"),
            ("portage", "/var/db/pkg"),
            ("eopkg", "/var/lib/eopkg"),
            ("slackpkg", "/var/log/packages"),
        ] {
            if Path::new(path).exists() {
                databases.push(manager.into());
            }
        }
        Self {
            nixos: id == "nixos" && Path::new("/run/current-system").exists(),
            guix: id == "guix" && Path::new("/run/current-system").exists(),
            dnf5: process::find("dnf5").is_some(),
            release,
            ostree,
            bootc,
            deployment_unknown,
            read_only_root,
            transactional,
            databases,
        }
    }

    pub fn system(&self) -> System {
        if self.deployment_unknown {
            return System::Unknown;
        }
        if self.bootc {
            return System::Bootc;
        }
        if self.ostree {
            return System::RpmOstree;
        }
        if self.transactional {
            return System::Transactional;
        }
        if self.nixos {
            return System::Nixos;
        }
        if self.guix {
            return System::Guix;
        }
        if self.read_only_root {
            return System::Unknown;
        }
        let ids = format!(
            "{} {}",
            self.release.get("ID").map(String::as_str).unwrap_or(""),
            self.release
                .get("ID_LIKE")
                .map(String::as_str)
                .unwrap_or("")
        );
        let has = |db: &str| self.databases.iter().any(|d| d == db);
        for id in ids.split_whitespace() {
            match id {
                "arch" | "manjaro" if has("pacman") => return System::Pacman,
                "debian" | "ubuntu" if has("apt") => return System::Apt,
                "opensuse-tumbleweed" | "opensuse-slowroll" if has("rpm") => {
                    return System::ZypperRolling
                }
                "opensuse-leap" | "opensuse" | "suse" | "sles" if has("rpm") => {
                    return System::ZypperLeap
                }
                "mageia" if has("rpm") => return System::Urpmi,
                "fedora" | "rhel" | "centos" | "openmandriva" if has("rpm") => {
                    return if self.dnf5 { System::Dnf5 } else { System::Dnf }
                }
                "alpine" if has("apk") => return System::Apk,
                "void" if has("xbps") => return System::Xbps,
                "gentoo" if has("portage") => return System::Portage,
                "solus" if has("eopkg") => return System::Eopkg,
                "slackware" if has("slackpkg") => return System::Slackpkg,
                _ => {}
            }
        }
        System::Unknown
    }
}

fn source(id: SourceId, provider: Provider, note: &str) -> Source {
    Source {
        id,
        provider,
        note: note.into(),
        items: vec![],
        excluded: vec![],
        error: None,
        checked: None,
        fresh: false,
        listed: false,
        executable: true,
        policy: None,
    }
}

fn home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(Into::into)
}

pub fn discover() -> Vec<Source> {
    let host = Host::read();
    let system = host.system();
    let mut sources = vec![source(
        SourceId::System,
        Provider::System(system),
        system.name(),
    )];
    match crate::policy::read() {
        Ok(policy) => match &policy.system {
            Some(crate::custom::Owner::Disabled) => sources.clear(),
            Some(crate::custom::Owner::Custom { id }) => match crate::custom::load(id) {
                Ok(provider) => {
                    let name = provider.manifest.name.clone();
                    sources = vec![source(SourceId::System, Provider::Custom(provider), &name)];
                }
                Err(error) => {
                    sources[0].executable = false;
                    sources[0].error = Some(error.to_string());
                    sources[0].note = error.to_string();
                }
            },
            _ => {}
        },
        Err(error) => {
            sources[0].executable = false;
            sources[0].error = Some(error.to_string());
            sources[0].note = error.to_string();
        }
    }
    // Preserve installations whose manager/service has gone missing. A source
    // disappearing during a failure would hide the fact that it wasn't checked.
    let user_flatpak = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| home().map(|p| p.join(".local/share")))
        .map(|p| p.join("flatpak"));
    if Path::new("/var/lib/flatpak/repo").exists()
        || Path::new("/etc/flatpak/installations.d").exists()
        || user_flatpak
            .as_ref()
            .is_some_and(|p| p.join("repo").exists())
    {
        sources.push(source(
            SourceId::Flatpak,
            Provider::Flatpak {
                installations: vec![],
            },
            "User and system installations",
        ));
    }
    if Path::new("/var/lib/snapd/state.json").exists() || Path::new("/var/lib/snapd/snaps").exists()
    {
        sources.push(source(
            SourceId::Snap,
            Provider::Snap,
            "snapd refreshes and holds",
        ));
    }
    if home().is_some_and(|p| {
        p.join(".nix-profile").exists() || p.join(".local/state/nix/profiles/profile").exists()
    }) {
        sources.push(source(
            SourceId::Nix,
            Provider::Nix,
            "Current user's independent Nix profile",
        ));
    }
    if home().is_some_and(|p| p.join(".guix-profile").exists()) {
        sources.push(source(
            SourceId::Guix,
            Provider::Guix,
            "Current user's Guix profile",
        ));
    }
    sources.push(source(
        SourceId::Firmware,
        Provider::Firmware,
        "fwupd · BIOS/UEFI excluded",
    ));
    // Second, after the system: listed only where some of the family is
    // installed in a way no repository of this system will ever update.
    if !crate::releases::find(&host).is_empty() {
        let after_system = usize::from(sources.first().is_some_and(|s| s.id == SourceId::System));
        sources.insert(
            after_system,
            source(
                SourceId::Linexinbar,
                Provider::Releases { operations: vec![] },
                "Released on GitHub · for what this system's repositories do not carry",
            ),
        );
    }
    sources
}

/// Ask a provider what it would change and keep the answer as items — one
/// a thing, read in the provider's own shape — so the count on the page is a
/// count of things. See [`crate::listing`].
fn preview(
    source: &mut Source,
    program: &str,
    args: &[&str],
    codes: &[i32],
    fresh: bool,
    listing: Listing,
) -> Result<()> {
    let output = process::probe(program, args, codes)?;
    source.items = listing::items(listing, &output.text);
    source.listed = listing.counted();
    source.fresh = fresh;
    source.note = if !listing.counted() {
        "Ready to update · what changes is known when it runs".into()
    } else if source.items.is_empty() && fresh {
        "No updates reported".into()
    } else if fresh {
        "Check complete · review provider details".into()
    } else {
        "Cached preview · the manager refreshes and confirms before installation".into()
    };
    Ok(())
}

pub fn check(source: &mut Source) {
    if source.error.is_some() && source.checked.is_none() {
        source.checked = Some(crate::now());
        return;
    }
    source.error = None;
    source.fresh = false;
    source.listed = false;
    source.items.clear();
    source.executable = true;
    let result = check_inner(source).and_then(|()| {
        if serde_json::to_vec(source)?.len() > 250_000 {
            source.items.clear();
            source.excluded.clear();
            bail!("The provider returned too much detail for this review. Use its native interface; this check is incomplete.");
        }
        Ok(())
    });
    match result {
        Ok(()) => source.checked = Some(now()),
        Err(error) => {
            source.error = Some(error.to_string());
            source.note = "Could not check — needs attention".into();
            source.executable = false;
        }
    }
}

fn check_inner(source: &mut Source) -> Result<()> {
    use System as S;
    match source.provider.clone() {
        Provider::Custom(provider) => {
            let checked = provider.check()?;
            source.note = format!("{}: {}", provider.manifest.name, checked.summary);
            source.items = checked.items;
            source.listed = checked.availability == crate::custom::Availability::Current
                || !source.items.is_empty();
            source.fresh = checked.availability != crate::custom::Availability::Unknown;
            source.executable = true;
            Ok(())
        }
        Provider::System(system) => match system {
            S::Pacman => preview(source, "checkupdates", &[], &[0, 2], true, Listing::Arrow),
            S::Apt => preview(
                source,
                "apt-get",
                &["--simulate", "dist-upgrade"],
                &[0],
                false,
                Listing::Apt,
            ),
            S::Dnf5 => preview(
                source,
                "dnf5",
                &["--refresh", "check-upgrade"],
                &[0, 100],
                true,
                Listing::Dnf,
            ),
            S::Dnf => preview(
                source,
                "dnf",
                &["--refresh", "check-update"],
                &[0, 100],
                true,
                Listing::Dnf,
            ),
            S::ZypperLeap | S::ZypperRolling => preview(
                source,
                "zypper",
                &["--no-refresh", "list-updates"],
                &[0, 100, 101],
                false,
                Listing::Zypper,
            ),
            S::RpmOstree => preview(
                source,
                "rpm-ostree",
                &["status"],
                &[0],
                false,
                Listing::Status,
            ),
            S::Bootc => preview(source, "bootc", &["status"], &[0], false, Listing::Status),
            S::Transactional => {
                preview(
                    source,
                    "zypper",
                    &["--no-refresh", "list-updates"],
                    &[0, 100, 101],
                    false,
                    Listing::Zypper,
                )?;
                source.note = "Cached preview · transactional-update stages the next snapshot using its configured update method".into();
                Ok(())
            }
            S::Apk => preview(
                source,
                "apk",
                &["version", "-l", "<"],
                &[0],
                false,
                Listing::Apk,
            ),
            S::Xbps => preview(source, "xbps-install", &["-un"], &[0], false, Listing::Xbps),
            S::Portage => preview(
                source,
                "emerge",
                &[
                    "--pretend",
                    "--verbose",
                    "--update",
                    "--deep",
                    "--newuse",
                    "@world",
                ],
                &[0],
                false,
                Listing::Portage,
            ),
            S::Eopkg => preview(
                source,
                "eopkg",
                &["list-upgrades"],
                &[0],
                false,
                Listing::Words,
            ),
            S::Urpmi => preview(
                source,
                "urpmq",
                &["--auto-select"],
                &[0],
                false,
                Listing::Words,
            ),
            S::Slackpkg => {
                source.executable = false;
                source.note =
                    "Manual maintenance · slackpkg requires its native full-screen interface"
                        .into();
                Ok(())
            }
            S::Nixos => {
                let policy = crate::policy::read()?;
                source.policy = Some(policy.clone());
                match policy.nixos {
                    Some(crate::policy::Nixos::Channels) => {
                        source.note = "Configured NixOS channels · update root's channels, build and stage the next boot generation".into();
                    }
                    Some(crate::policy::Nixos::Flake {
                        directory,
                        configuration,
                    }) => {
                        source.note = format!("Configured NixOS flake {directory}#{configuration} · update flake.lock and stage the next boot generation");
                        source.items.push(Item { name: "Configuration input changes".into(), detail: "Continue authorizes updating this flake's lock file. The system generation is built before the next boot is changed.".into() });
                    }
                    None => {
                        source.executable = false;
                        source.note = "Needs configuration · select the NixOS source in /etc/linexinbar/updates.json".into();
                    }
                }
                Ok(())
            }
            S::Guix => {
                let policy = crate::policy::read()?;
                source.policy = Some(policy.clone());
                if let Some(file) = policy.guix_system {
                    source.note = format!("Configured Guix system {file} · update root's channels, build and reconfigure");
                } else {
                    source.executable = false;
                    source.note = "Needs configuration · select the Guix system file in /etc/linexinbar/updates.json".into();
                }
                Ok(())
            }
            S::Unknown => bail!("No validated provider owns this running system"),
        },
        Provider::Flatpak { .. } => {
            let installations = process::probe("flatpak", &["--installations"], &[0])?;
            // --installation takes an ID, while --installations reports paths.
            // Ask the listing for the exact installation IDs, not basenames.
            let listing = process::probe(
                "flatpak",
                &["list", "--columns=installation", "--all"],
                &[0],
            )?;
            let mut scopes: Vec<String> = listing
                .text
                .lines()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            scopes.sort();
            scopes.dedup();
            if scopes
                .iter()
                .any(|s| s.starts_with('-') || s.contains('/') || s.chars().any(char::is_control))
            {
                bail!("Unrecognized Flatpak installation ID");
            }
            let _ = installations;
            source.provider = Provider::Flatpak {
                installations: scopes.clone(),
            };
            let mut items = vec![];
            for scope in scopes {
                let flag = scope_flag(&scope);
                let output = process::probe(
                    "flatpak",
                    &["remote-ls", "--updates", "--columns=ref,version", &flag],
                    &[0],
                )?;
                items.extend(
                    listing::items(Listing::FlatpakRef, &output.text)
                        .into_iter()
                        .map(|mut item| {
                            item.detail = format!("{} · {scope}", item.detail);
                            item
                        }),
                );
            }
            source.items = items;
            source.listed = true;
            source.fresh = true;
            source.note =
                "Flatpak applications, runtimes and extensions · all installed scopes".into();
            Ok(())
        }
        Provider::Releases { .. } => crate::releases::check(source),
        Provider::Aur { .. } => bail!("AUR packages must be updated manually"),
        Provider::Snap => preview(
            source,
            "snap",
            &["refresh", "--list"],
            &[0],
            true,
            Listing::Snap,
        ),
        Provider::Nix => {
            let policy = crate::policy::read()?;
            source.policy = Some(policy.clone());
            source.executable = policy.independent_nix_profile;
            let inventory = process::probe("nix", &["profile", "list", "--json"], &[0])?;
            if inventory.text.contains("home-manager-path") {
                source.executable = false;
            }
            source.note = if source.executable {
                "Independent Nix profile · upgrade unlocked inputs; pinned packages retain their inputs"
            } else { "Needs configuration · declare independent_nix_profile in /etc/linexinbar/updates.json; Home Manager profiles remain external" }.into();
            if source.executable {
                let help = process::probe("nix", &["profile", "upgrade", "--help"], &[0])?;
                if !help.text.contains("--all") {
                    bail!("This Nix version needs its native profile maintenance workflow");
                }
            }
            Ok(())
        }
        Provider::Guix => {
            let policy = crate::policy::read()?;
            source.policy = Some(policy.clone());
            source.executable = policy.independent_guix_profile;
            process::probe("guix", &["package", "--list-installed"], &[0])?;
            source.note = if source.executable { "Independent Guix profile · pull configured channels and upgrade packages" }
                else { "Needs configuration · declare independent_guix_profile in /etc/linexinbar/updates.json; manifest-owned profiles remain external" }.into();
            Ok(())
        }
        Provider::AppImage => {
            source.executable = false;
            source.note = "Manual maintenance · no verified update mechanism".into();
            Ok(())
        }
        Provider::Firmware => {
            let refreshed = firmware::refresh();
            let (items, excluded) = firmware::inventory()?;
            source.items = items;
            source.excluded = excluded;
            source.listed = true;
            source.fresh = refreshed?;
            source.note = if source.fresh {
                "fwupd metadata refreshed · BIOS/UEFI and unclassified devices excluded"
            } else {
                "Cached fwupd metadata · no new metadata downloaded; BIOS/UEFI and unclassified devices excluded"
            }.into();
            Ok(())
        }
    }
}

pub fn scope_flag(scope: &str) -> String {
    match scope {
        "user" => "--user".into(),
        "system" => "--system".into(),
        _ => format!("--installation={scope}"),
    }
}

/// The commands that install what a source listed, unattended.
///
/// Unattended on purpose. Update now on the review *is* the confirmation —
/// the person has read the count and pressed the one button that installs
/// it — and a tool that then stops to ask "Proceed with installation?
/// [Y/n]" is asking the same question a second time, and a third when the
/// next tool asks its own. So every recipe carries its tool's own flag for
/// answering yes to its ordinary confirmation: pacman's `--noconfirm`,
/// APT's `-y`, DNF's `-y`, zypper's `--non-interactive`, and so on down the
/// table. What that flag does *not* answer stays a question the panel puts
/// up — a conffile dpkg cannot decide about, a sudo password an AUR helper
/// asks for — and the panel answers those with buttons and a field.
///
/// Never `--force`, never a reboot, never an unsigned package or an erased
/// dependency: the flag answers the yes-or-no the tool would have asked,
/// and nothing the tool would not have offered.
pub fn steps(source: &Source) -> Result<Vec<Step>> {
    use System as S;
    if !source.executable || source.error.is_some() {
        bail!("{} requires attention", source.id.title());
    }
    if let Some(reviewed) = &source.policy {
        if &crate::policy::read()? != reviewed {
            bail!("The configured update source changed. Check and review again.");
        }
    }
    let steps = match &source.provider {
        Provider::Custom(provider) => vec![provider.step()?],
        Provider::System(system) => match system {
            S::Nixos => match source.policy.as_ref().and_then(|p| p.nixos.as_ref()) {
                Some(crate::policy::Nixos::Channels) => vec![
                    Step::new("nix-channel", &["--update"], true),
                    Step::new("nixos-rebuild", &["build"], true),
                    Step::new("nixos-rebuild", &["boot"], true).staged(),
                ],
                Some(crate::policy::Nixos::Flake {
                    directory,
                    configuration,
                }) => {
                    let target = format!("{directory}#{configuration}");
                    vec![
                        Step::new("nix", &["flake", "update", "--flake", directory], true),
                        Step::new("nixos-rebuild", &["build", "--flake", &target], true),
                        Step::new("nixos-rebuild", &["boot", "--flake", &target], true).staged(),
                    ]
                }
                None => bail!("Configure the NixOS update source first"),
            },
            S::Guix => {
                let file = source
                    .policy
                    .as_ref()
                    .and_then(|p| p.guix_system.as_deref())
                    .ok_or_else(|| anyhow::anyhow!("Configure the Guix system source first"))?;
                vec![
                    Step::new("guix", &["pull"], true),
                    Step::new("guix", &["system", "build", file], true).pulled_guix(),
                    Step::new("guix", &["system", "reconfigure", file], true).pulled_guix(),
                ]
            }
            S::Pacman => vec![Step::new("pacman", &["-Syu", "--noconfirm"], true)],
            // dpkg's conffile question is the one `-y` does not answer;
            // `--force-confdef` takes the default where there is one and
            // `--force-confold` keeps the administrator's file where there
            // is not, which is what unattended-upgrades does too. Debconf
            // questions fall to its plain-text frontend on this PTY — the
            // dialog one refuses a dumb terminal — and reach the panel as
            // typed prompts.
            S::Apt => vec![
                Step::new("apt-get", &["update"], true),
                Step::new(
                    "apt-get",
                    &[
                        "-y",
                        "-o",
                        "Dpkg::Options::=--force-confdef",
                        "-o",
                        "Dpkg::Options::=--force-confold",
                        "dist-upgrade",
                    ],
                    true,
                ),
            ],
            S::Dnf5 => {
                vec![Step::new("dnf5", &["-y", "--refresh", "upgrade", "--offline"], true).staged()]
            }
            S::Dnf => vec![Step::new("dnf", &["-y", "--refresh", "upgrade"], true)],
            S::ZypperLeap => vec![
                Step::new("zypper", &["--non-interactive", "refresh"], true),
                Step::new("zypper", &["--non-interactive", "patch"], true),
                Step::new("zypper", &["--non-interactive", "update"], true),
            ],
            S::ZypperRolling => vec![
                Step::new("zypper", &["--non-interactive", "refresh"], true),
                Step::new("zypper", &["--non-interactive", "dup"], true),
            ],
            S::RpmOstree => vec![Step::new("rpm-ostree", &["upgrade"], false).staged()],
            S::Bootc => vec![Step::new("bootc", &["upgrade"], true).staged()],
            // No command: the method configured in transactional-update.conf,
            // which is what the check's note says it stages.
            S::Transactional => {
                vec![Step::new("transactional-update", &["--non-interactive"], true).staged()]
            }
            S::Apk => vec![
                Step::new("apk", &["update"], true),
                Step::new("apk", &["upgrade"], true),
            ],
            // Twice, as xbps asks: the first run updates xbps itself when it
            // is among the updates and stops there.
            S::Xbps => vec![
                Step::new("xbps-install", &["-Suy"], true),
                Step::new("xbps-install", &["-uy"], true),
            ],
            S::Portage => vec![
                Step::new("emerge", &["--sync"], true),
                Step::new(
                    "emerge",
                    &["--verbose", "--update", "--deep", "--newuse", "@world"],
                    true,
                ),
            ],
            S::Eopkg => vec![Step::new("eopkg", &["upgrade", "-y"], true)],
            // --auto-select chooses the upgrade set; --auto answers the
            // questions about it.
            S::Urpmi => vec![
                Step::new("urpmi.update", &["-a"], true),
                Step::new("urpmi", &["--auto-select", "--auto"], true),
            ],
            _ => bail!("This system requires its configured native maintenance workflow"),
        },
        Provider::Flatpak { installations } => installations
            .iter()
            .map(|s| Step::new("flatpak", &["update", "-y", &scope_flag(s)], false))
            .collect(),
        // The helper's review of each PKGBUILD is skipped along with its
        // confirmations: it is a review for somebody at a terminal with an
        // editor, and this is a press on a panel. The helper's sudo asks for
        // its password on the PTY, which the panel opens its field for.
        Provider::Aur { .. } => bail!("AUR packages must be updated manually"),
        Provider::Releases { operations } => crate::releases::steps(operations)?,
        Provider::Snap => vec![Step::new("snap", &["refresh"], true)],
        Provider::Nix => vec![Step::new("nix", &["profile", "upgrade", "--all"], false)],
        Provider::Guix => vec![
            Step::new("guix", &["pull"], false),
            Step::new("guix", &["package", "--upgrade"], false).pulled_guix(),
        ],
        Provider::Firmware => {
            bail!("Firmware steps must be classified immediately before each device installation")
        }
        _ => bail!("This source requires manual maintenance"),
    };
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn host(id: &str, like: &str, database: &str) -> Host {
        Host {
            release: os_release(&format!("ID={id}\nID_LIKE=\"{like}\"")),
            databases: vec![database.into()],
            ..Host::default()
        }
    }
    #[test]
    fn derivatives_require_both_ancestry_and_host_database() {
        assert_eq!(host("linexin", "arch", "pacman").system(), System::Pacman);
        assert_eq!(host("ubuntu", "debian", "pacman").system(), System::Unknown);
        assert_eq!(host("mint", "ubuntu debian", "apt").system(), System::Apt);
        assert_eq!(
            host("opensuse-tumbleweed", "opensuse suse", "rpm").system(),
            System::ZypperRolling
        );
        assert_eq!(
            host("opensuse-leap", "suse", "rpm").system(),
            System::ZypperLeap
        );
    }
    #[test]
    fn image_ownership_precedes_rpm_tools() {
        let mut h = host("fedora", "", "rpm");
        h.ostree = true;
        assert_eq!(h.system(), System::RpmOstree);
        h.bootc = true;
        assert_eq!(h.system(), System::Bootc);
        h.deployment_unknown = true;
        assert_eq!(h.system(), System::Unknown);
        let mut h = host("unrecognized-immutable", "debian", "apt");
        h.read_only_root = true;
        assert_eq!(h.system(), System::Unknown);
        let mut h = host("opensuse-microos", "suse", "rpm");
        h.transactional = true;
        assert_eq!(h.system(), System::Transactional);
    }
    /// Every recipe answers its tool's own confirmation — Update now was
    /// the confirmation — and none of them forces, erases, downgrades or
    /// reboots anything the tool would not have offered.
    #[test]
    fn updates_are_unattended_and_never_reboot() {
        for (system, unattended) in [
            (System::Pacman, "--noconfirm"),
            (System::Apt, "-y"),
            (System::Dnf5, "-y"),
            (System::Dnf, "-y"),
            (System::ZypperLeap, "--non-interactive"),
            (System::ZypperRolling, "--non-interactive"),
            (System::Apk, ""),
            (System::Xbps, "-Suy"),
            (System::Portage, ""),
            (System::Eopkg, "-y"),
            (System::Urpmi, "--auto"),
            (System::RpmOstree, ""),
            (System::Bootc, ""),
            (System::Transactional, "--non-interactive"),
        ] {
            let s = source(SourceId::System, Provider::System(system), "");
            let steps = steps(&s).unwrap();
            for step in &steps {
                for forbidden in [
                    "--force",
                    "--allow-unauthenticated",
                    "--allow-downgrades",
                    "--reboot",
                    "--apply",
                    "--allowerasing",
                    "--ask",
                    "--interactive",
                ] {
                    assert!(!step.args.iter().any(|a| a == forbidden), "{step:?}");
                }
            }
            if !unattended.is_empty() {
                assert!(
                    steps
                        .iter()
                        .any(|step| step.args.iter().any(|a| a == unattended)),
                    "{system:?} runs {steps:?} and never says {unattended}"
                );
            }
        }
        assert_eq!(
            steps(&source(
                SourceId::System,
                Provider::System(System::Pacman),
                ""
            ))
            .unwrap()[0]
                .args,
            ["-Syu", "--noconfirm"]
        );
        for helper in ["paru", "yay"] {
            let s = source(
                SourceId::Aur,
                Provider::Aur {
                    helper: Some(helper.into()),
                },
                "",
            );
            assert!(steps(&s).is_err(), "{helper} must never execute");
        }
    }
    #[test]
    fn flatpak_scopes_do_not_collapse() {
        let s = source(
            SourceId::Flatpak,
            Provider::Flatpak {
                installations: vec!["user".into(), "system".into(), "games".into()],
            },
            "",
        );
        let commands = steps(&s).unwrap();
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[2].args, ["update", "-y", "--installation=games"]);
        assert!(commands.iter().all(|s| !s.root));
    }
}
