//! What a provider's preview lists, one item a thing.
//!
//! A check used to keep every non-empty line of a preview as an item, so the
//! number on the page was a number of *lines*: an APT simulation's "Reading
//! package lists…" and "Building dependency tree…" were two updates, DNF's
//! "Last metadata expiration check" was one, and snap's column header was
//! another. The page could only say "Updates available" and send the reader
//! to Details for a list that was mostly not packages. Each provider writes
//! its list in a shape of its own, and this is where each shape is read, so
//! that `items.len()` is the count the page puts up.
//!
//! A line that is not a thing is dropped rather than kept as a mystery item.
//! A provider whose preview is not a list at all — an rpm-ostree deployment's
//! status, a Nix profile's inventory — is [`Listing::Status`], lists nothing,
//! and is not counted; the source says so through [`crate::Source::listed`].
use crate::Item;

/// The shape a provider writes its preview in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Listing {
    /// `name old -> new`: checkupdates, and paru or yay `-Qua`.
    Arrow,
    /// `Inst name [old] (new repo …)` and `Remv name [old]`, among the
    /// `Conf` lines and the talk before them: `apt-get --simulate`.
    Apt,
    /// `name.arch  version  repo` under a line or two about metadata: DNF
    /// and DNF5's check.
    Dnf,
    /// `v | repo | name | old | new | arch`, under a header and a rule:
    /// zypper's list-updates.
    Zypper,
    /// `name-old < new` under an `Installed: Available:` header: `apk
    /// version -l '<'`.
    Apk,
    /// `name-ver update arch url …`: `xbps-install -un`.
    Xbps,
    /// `[ebuild  U ] cat/name-ver [old] …`: `emerge --pretend`.
    Portage,
    /// `Name Version Rev …` as a header, then one snap a line: `snap refresh
    /// --list`.
    Snap,
    /// `kind/id/arch/branch<TAB>version`: `flatpak remote-ls --updates`.
    FlatpakRef,
    /// One thing a line, the first word its name: eopkg, urpmq.
    Words,
    /// Not a list: a deployment's status, a profile's inventory. Nothing is
    /// counted, and the page says the source is ready rather than how many.
    Status,
}

impl Listing {
    /// Whether what this shape lists can be counted.
    pub fn counted(self) -> bool {
        self != Self::Status
    }
}

/// The items in `text`, read in `listing`'s shape.
pub fn items(listing: Listing, text: &str) -> Vec<Item> {
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| match listing {
            Listing::Arrow => arrow(line),
            Listing::Apt => apt(line),
            Listing::Dnf => dnf(line),
            Listing::Zypper => zypper(line),
            Listing::Apk => apk(line),
            Listing::Xbps => xbps(line),
            Listing::Portage => portage(line),
            Listing::Snap => snap(line),
            Listing::FlatpakRef => flatpak(line),
            Listing::Words => words(line),
            Listing::Status => None,
        })
        .collect()
}

fn item(name: &str, detail: String) -> Option<Item> {
    (!name.is_empty()).then(|| Item {
        name: name.to_owned(),
        detail,
    })
}

fn arrow(line: &str) -> Option<Item> {
    let mut words = line.split_whitespace();
    let (name, old, arrow, new) = (words.next()?, words.next()?, words.next()?, words.next()?);
    if arrow != "->" {
        return None;
    }
    item(name, format!("{old} → {new}"))
}

fn apt(line: &str) -> Option<Item> {
    let mut words = line.split_whitespace();
    let verb = words.next()?;
    let name = words.next()?;
    let rest: Vec<&str> = words.collect();
    let old = rest
        .first()
        .and_then(|w| w.strip_prefix('['))
        .map(|w| w.trim_end_matches(']'));
    match verb {
        "Inst" => {
            let new = rest
                .iter()
                .find_map(|w| w.strip_prefix('('))
                .unwrap_or_default();
            item(
                name,
                match old {
                    Some(old) => format!("{old} → {new}"),
                    None => format!("{new} · new"),
                },
            )
        }
        "Remv" => item(
            name,
            match old {
                Some(old) => format!("{old} · removed"),
                None => "removed".into(),
            },
        ),
        _ => None,
    }
}

fn dnf(line: &str) -> Option<Item> {
    // A package stands at the margin; the packages an obsoleting one
    // replaces are listed indented under it, and they are not updates.
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    let [name_arch, version, _repo] = words[..] else {
        return None;
    };
    let (name, _arch) = name_arch.rsplit_once('.')?;
    item(name, version.to_owned())
}

fn zypper(line: &str) -> Option<Item> {
    if line.starts_with('-') {
        return None;
    }
    let fields: Vec<&str> = line.split('|').map(str::trim).collect();
    if fields.len() < 5 || fields[2] == "Name" {
        return None;
    }
    item(fields[2], format!("{} → {}", fields[3], fields[4]))
}

/// Where a `name-1.2.3` style name stops and its version starts: the first
/// dash that has a digit after it.
fn version_starts(name_version: &str) -> Option<usize> {
    name_version
        .match_indices('-')
        .find(|(at, _)| {
            name_version[at + 1..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        })
        .map(|(at, _)| at)
}

fn apk(line: &str) -> Option<Item> {
    let (installed, available) = line.split_once(" < ")?;
    let installed = installed.trim();
    let at = version_starts(installed)?;
    item(
        &installed[..at],
        format!("{} → {}", &installed[at + 1..], available.trim()),
    )
}

fn xbps(line: &str) -> Option<Item> {
    let mut words = line.split_whitespace();
    let package = words.next()?;
    let action = words.next()?;
    if !matches!(action, "update" | "install") {
        return None;
    }
    let (name, version) = package.rsplit_once('-')?;
    item(
        name,
        if action == "install" {
            format!("{version} · new")
        } else {
            version.to_owned()
        },
    )
}

fn portage(line: &str) -> Option<Item> {
    if !(line.starts_with("[ebuild") || line.starts_with("[binary")) {
        return None;
    }
    let after = line.split_once("] ")?.1;
    let mut words = after.split_whitespace();
    let atom = words.next()?;
    let at = version_starts(atom)?;
    let (name, new) = (&atom[..at], &atom[at + 1..]);
    let old = words
        .next()
        .and_then(|w| w.strip_prefix('['))
        .map(|w| w.trim_end_matches(']'));
    item(
        name,
        match old {
            Some(old) => format!("{old} → {new}"),
            None => new.to_owned(),
        },
    )
}

fn snap(line: &str) -> Option<Item> {
    let mut words = line.split_whitespace();
    let name = words.next()?;
    let version = words.next().unwrap_or_default();
    if name == "Name" && version == "Version" || line.contains("up to date") {
        return None;
    }
    item(name, version.to_owned())
}

fn flatpak(line: &str) -> Option<Item> {
    let mut columns = line.split(['\t', ' ']).filter(|c| !c.is_empty());
    let reference = columns.next()?;
    let version = columns.next().unwrap_or_default();
    let mut segments = reference.split('/');
    let kind = segments.next()?;
    let id = segments.next()?;
    let branch = segments.nth(1).unwrap_or_default();
    item(
        id,
        match kind {
            "runtime" => format!("{branch} runtime"),
            _ if version.is_empty() => branch.to_owned(),
            _ => version.to_owned(),
        },
    )
}

fn words(line: &str) -> Option<Item> {
    let trimmed = line.trim_end();
    if line.starts_with(char::is_whitespace)
        || trimmed.ends_with(['.', ':'])
        || trimmed.contains('"')
    {
        return None;
    }
    let (name, rest) = trimmed
        .split_once(char::is_whitespace)
        .unwrap_or((trimmed, ""));
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-' | ':'))
    {
        return None;
    }
    item(name, rest.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(listing: Listing, text: &str) -> Vec<(String, String)> {
        items(listing, text)
            .into_iter()
            .map(|i| (i.name, i.detail))
            .collect()
    }

    /// The talk around a list is not part of it: what APT says before its
    /// first `Inst`, what DNF says about its metadata, snap's header. This is
    /// the difference between "4 updates" and "Updates available".
    #[test]
    fn a_preview_counts_things_and_not_lines() {
        let apt = "Reading package lists...\nBuilding dependency tree...\nReading state information...\nCalculating upgrade...\nInst libc6 [2.40-3] (2.40-4 Debian:13/stable [amd64])\nConf libc6 (2.40-4 Debian:13/stable [amd64])\nInst new-thing (1.0-1 Debian:13/stable [amd64])\nRemv old-thing [0.9-1]\n";
        assert_eq!(
            names(Listing::Apt, apt),
            [
                ("libc6".into(), "2.40-3 → 2.40-4".into()),
                ("new-thing".into(), "1.0-1 · new".into()),
                ("old-thing".into(), "0.9-1 · removed".into()),
            ]
        );
        let dnf = "Updating and loading repositories:\nRepositories loaded.\nkernel.x86_64  6.17.4-200.fc43  updates\npython3.12.x86_64  3.12.8-1.fc43  updates\nObsoleting Packages\nnew.x86_64  2-1  updates\n    old.x86_64  1-1  @fedora\n";
        assert_eq!(
            names(Listing::Dnf, dnf),
            [
                ("kernel".into(), "6.17.4-200.fc43".into()),
                ("python3.12".into(), "3.12.8-1.fc43".into()),
                ("new".into(), "2-1".into()),
            ]
        );
        let snap = "Name      Version  Rev   Size   Publisher   Notes\nfirefox   131.0-2  5000  280MB  mozilla✓    -\n";
        assert_eq!(
            names(Listing::Snap, snap),
            [("firefox".into(), "131.0-2".into())]
        );
        assert!(names(Listing::Snap, "All snaps up to date.").is_empty());
    }

    #[test]
    fn each_shape_yields_a_name_and_what_changes() {
        assert_eq!(
            names(Listing::Arrow, "linux 6.17.3-1 -> 6.17.4-1\n"),
            [("linux".into(), "6.17.3-1 → 6.17.4-1".into())]
        );
        assert!(names(Listing::Arrow, ":: Checking for updates...").is_empty());
        let zypper = "Loading repository data...\nS | Repository | Name  | Current Version | Available Version | Arch\n--+------------+-------+-----------------+-------------------+-------\nv | Main       | bash  | 5.2-1           | 5.2-2             | x86_64\n";
        assert_eq!(
            names(Listing::Zypper, zypper),
            [("bash".into(), "5.2-1 → 5.2-2".into())]
        );
        let apk = "Installed:                 Available:\nbusybox-1.36.1-r0          < 1.36.1-r1\n";
        assert_eq!(
            names(Listing::Apk, apk),
            [("busybox".into(), "1.36.1-r0 → 1.36.1-r1".into())]
        );
        assert_eq!(
            names(
                Listing::Xbps,
                "bash-5.2.21_1 update x86_64 https://repo 1 2\nnew-1.0_1 install x86_64 https://repo 1 2\n"
            ),
            [
                ("bash".into(), "5.2.21_1".into()),
                ("new".into(), "1.0_1 · new".into())
            ]
        );
        let portage = "These are the packages that would be merged:\n[ebuild     U  ] sys-apps/coreutils-9.5-r1 [9.4] USE=\"acl\"\n[blocks b      ] <sys-libs/old-1\n";
        assert_eq!(
            names(Listing::Portage, portage),
            [("sys-apps/coreutils".into(), "9.4 → 9.5-r1".into())]
        );
        assert_eq!(
            names(
                Listing::FlatpakRef,
                "app/org.example.Player/x86_64/stable\t1.2.0\nruntime/org.freedesktop.Platform/x86_64/24.08\t24.08.14\n"
            ),
            [
                ("org.example.Player".into(), "1.2.0".into()),
                ("org.freedesktop.Platform".into(), "24.08 runtime".into())
            ]
        );
        assert_eq!(
            names(Listing::Words, "nano 8.2-1\nNo packages to upgrade.\n"),
            [("nano".into(), "8.2-1".into())]
        );
        assert!(names(Listing::Status, "State: idle\nDeployments:\n").is_empty());
        assert!(!Listing::Status.counted());
    }
}
