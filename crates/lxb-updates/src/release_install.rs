//! `lxb-updates release <operation>`: what a reviewed [`Operation`] does, in
//! the job's terminal.
//!
//! Run as root by the authorized job worker — or as the person, for a build
//! installed into their own home — and trusting nothing it was handed beyond
//! the operation's own shape. It asks GitHub for the release itself, refuses a
//! version that is not newer than what is installed, checks every downloaded
//! file against the SHA-256 GitHub published for it, and builds in a directory
//! only its own account can write. So a review that was tampered with on its
//! way here can at most ask for a newer release of something that is already
//! installed, which is what Update now was pressed for anyway.
use crate::process;
use crate::releases::{self, Format, Operation, Record, Target, Version};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

/// Where root keeps what it downloads and builds. Its own, under /var, where
/// no ordinary account can put anything.
const CACHE: &str = "/var/cache/lxb-updates";

/// No release file of this family is anywhere near this; a download that
/// runs past it is not one of them.
const LARGEST: u64 = 1 << 30;

pub fn run(argument: &str) -> Result<()> {
    let operation: Operation =
        serde_json::from_str(argument).context("Not a release operation this helper knows")?;
    let root = unsafe { libc::geteuid() } == 0;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    operation.validate(if root { None } else { home.as_deref() })?;
    if operation.root() != root {
        bail!(if root {
            "A build into a home directory is never run as root"
        } else {
            "Installing this needs the authorized update job"
        });
    }
    match operation {
        Operation::Packages { targets } => packages(&targets),
        Operation::Source {
            repo,
            tag,
            prefix,
            components,
            libdir,
            sitedir,
        } => source(
            &repo,
            &tag,
            Path::new(&prefix),
            &components,
            libdir.as_deref(),
            sitedir.as_deref(),
            root,
        ),
    }
}

fn say(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{text}");
    let _ = out.flush();
}

// ───────────────────────────── packages ─────────────────────────────

fn packages(targets: &[Target]) -> Result<()> {
    let host = crate::discovery::Host::read();
    let format = Format::of(&host, host.system())
        .context("No release carries packages for this system's package manager")?;
    let arch = format.arch();
    let installed = foreign_packages();
    let total: usize = targets.iter().map(|t| t.packages.len()).sum();
    let directory = fresh(&Path::new(CACHE).join("packages"), 0o755)?;
    let result = (|| -> Result<()> {
        let mut files = vec![];
        let mut done = 0;
        for target in targets {
            let release = releases::release(&target.repo, &target.tag)?;
            for name in &target.packages {
                let have = installed.get(name).with_context(|| {
                    format!("{name} is not installed as a package no repository offers, so it is not this source's to update")
                })?;
                let package = releases::package_for(format, &arch, &release.assets, name)
                    .with_context(|| {
                        format!(
                            "{} {} has no {name} for this system",
                            target.repo, target.tag
                        )
                    })?;
                if releases::vercmp(&package.version, have) != Ordering::Greater {
                    bail!(
                        "{name} {} is not newer than the installed {have}; nothing is downgraded",
                        package.version
                    );
                }
                done += 1;
                // "of", not a slash: the bar is the package manager's to
                // move, and a count of downloads would fill it before
                // anything was installed.
                say(&format!(
                    "Downloading {} · {} ({done} of {total})",
                    package.file,
                    megabytes(package.size)
                ));
                let path = directory.join(&package.file);
                download(&target.repo, &target.tag, &package, &path)?;
                files.push(path);
            }
        }
        say("Every file matches the checksum GitHub published for it.");
        let status = install(format, &files)?;
        if !status.success() {
            bail!("The package manager exited {}", status.code().unwrap_or(-1));
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(&directory);
    result
}

/// The packages this source may update, with their versions: asked again
/// here rather than taken from the review, which may be half an hour old and
/// was carried here by a process that is not root. A package a repository
/// has started carrying since is the system's again.
fn foreign_packages() -> std::collections::BTreeMap<String, String> {
    let mut versions = std::collections::BTreeMap::new();
    for found in releases::find(&crate::discovery::Host::read()) {
        if let releases::Found::Packaged { packages, .. } = found {
            versions.extend(packages);
        }
    }
    versions
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// Fetch one release file to `path`, checking it against the digest GitHub
/// published for it as it arrives.
fn download(repo: &str, tag: &str, package: &releases::Package, path: &Path) -> Result<()> {
    let expected = package
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|d| d.len() == 64 && d.bytes().all(|b| b.is_ascii_hexdigit()))
        .with_context(|| format!("GitHub publishes no checksum for {}", package.file))?
        .to_ascii_lowercase();
    if package.file.is_empty()
        || package.file.starts_with('.')
        || package.file.contains('/')
        || package.file.chars().any(char::is_control)
    {
        bail!("{:?} is not a file name", package.file);
    }
    let origin = format!(
        "https://github.com/{}/{repo}/releases/download/{tag}/",
        releases::OWNER
    );
    if !package.url.starts_with(&origin) || package.url[origin.len()..].contains('/') {
        bail!("{} is not a file of {repo} {tag}", package.file);
    }
    if package.size > LARGEST {
        bail!("{} is larger than any release file should be", package.file);
    }
    let mut response = releases::agent()
        .get(&package.url)
        .call()
        .map_err(|error| anyhow::anyhow!("{} could not be downloaded: {error}", package.file))?;
    let status = response.status().as_u16();
    if status != 200 {
        bail!("GitHub answered {status} for {}", package.file);
    }
    let mut reader = response.body_mut().with_config().limit(LARGEST).reader();
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    // A file that did not arrive whole and exactly as published is not left
    // lying where a package manager is about to be pointed.
    let received = receive(&mut reader, file, &expected, package);
    if received.is_err() {
        let _ = fs::remove_file(path);
    }
    received
}

fn receive(
    reader: &mut impl Read,
    mut file: fs::File,
    expected: &str,
    package: &releases::Package,
) -> Result<()> {
    let mut digest = Sha256::new();
    let mut length = 0u64;
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        length += n as u64;
        digest.update(&buffer[..n]);
        file.write_all(&buffer[..n])?;
    }
    file.sync_all()?;
    let actual = format!("{:x}", digest.finalize());
    if actual != expected || (package.size != 0 && length != package.size) {
        bail!(
            "{} does not match the checksum GitHub published for it; nothing was installed",
            package.file
        );
    }
    Ok(())
}

/// The system's own package manager, over every file at once — one
/// transaction, so that packages locked to one another's versions go in
/// together. Unattended, as every other source's tool is: Update now was the
/// confirmation. Never a flag that forces what the tool would refuse.
fn install(format: Format, files: &[PathBuf]) -> Result<ExitStatus> {
    let files: Vec<&str> = files
        .iter()
        .map(|p| p.to_str().context("Unreadable download path"))
        .collect::<Result<_>>()?;
    let (program, mut args): (&str, Vec<&str>) = match format {
        Format::Pacman => ("pacman", vec!["-U", "--noconfirm"]),
        Format::Deb => (
            "apt-get",
            vec![
                "install",
                "-y",
                "-o",
                "Dpkg::Options::=--force-confdef",
                "-o",
                "Dpkg::Options::=--force-confold",
            ],
        ),
        Format::Rpm { dnf5: true, .. } => ("dnf5", vec!["install", "-y"]),
        Format::Rpm { dnf5: false, .. } => ("dnf", vec!["install", "-y"]),
    };
    args.extend(&files);
    say(&format!("{program} {}", args.join(" ")));
    Ok(tool(program)?.args(&args).status()?)
}

/// A program off the fixed path this helper runs with, and — as root —
/// only from a protected installation, as every other root step is.
fn tool(program: &str) -> Result<Command> {
    let path = process::find(program).with_context(|| format!("{program} is not installed"))?;
    let path = if unsafe { libc::geteuid() } == 0 {
        process::trusted(path)?
    } else {
        path
    };
    Ok(Command::new(path))
}

/// An empty directory of this account's for one job, under `parent`.
fn fresh(parent: &Path, mode: u32) -> Result<PathBuf> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(parent)?;
    let meta = fs::symlink_metadata(parent)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
        bail!("{} is not this account's own directory", parent.display());
    }
    let name = format!("{}-{}", crate::now(), std::process::id());
    let directory = parent.join(name);
    fs::DirBuilder::new().mode(mode).create(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(mode))?;
    Ok(directory)
}

// ───────────────────────────── source ─────────────────────────────

#[allow(clippy::too_many_arguments)]
fn source(
    repo: &str,
    tag: &str,
    prefix: &Path,
    components: &[String],
    libdir: Option<&str>,
    sitedir: Option<&str>,
    root: bool,
) -> Result<()> {
    let project = releases::project(repo).context("Unknown project")?;
    let system = crate::discovery::Host::read().system();
    let built = releases::built(project, prefix, system).with_context(|| {
        format!(
            "{repo} is not installed from source under {}",
            prefix.display()
        )
    })?;
    if components
        .iter()
        .any(|c| !built.components.contains(&c.as_str()))
        || libdir.map(Path::new) != built.libdir.as_deref()
        || sitedir.map(Path::new) != built.sitedir.as_deref()
    {
        bail!("What is installed of {repo} changed since the check. Check again.");
    }
    let installed = releases::built_version(&built)
        .with_context(|| format!("The installed version of {repo} could not be read"))?;
    let wanted = Version::parse(tag).context("Invalid tag")?;
    if wanted <= installed {
        bail!("{repo} {tag} is not newer than the installed {installed}; nothing is downgraded");
    }
    releases::release(repo, tag)?;
    let parent = if root {
        Path::new(CACHE).join("build")
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .context("No cache directory")?
            .join("lxb/updates/build")
    };
    let work = fresh(&parent, 0o700)?;
    let result = build_and_install(repo, tag, prefix, components, libdir, sitedir, root, &work);
    let _ = fs::remove_dir_all(&work);
    result
}

#[allow(clippy::too_many_arguments)]
fn build_and_install(
    repo: &str,
    tag: &str,
    prefix: &Path,
    components: &[String],
    libdir: Option<&str>,
    sitedir: Option<&str>,
    root: bool,
    work: &Path,
) -> Result<()> {
    let tree = work.join("src");
    let target = work.join("target");
    let stage = work.join("stage");
    // Fractions with a verb in them, which is what the panel's bar counts.
    say(&format!("(1/3) Updating {repo}: fetching {tag}"));
    let url = format!("https://github.com/{}/{repo}.git", releases::OWNER);
    let mut git = tool("git")?;
    git.args([
        "-c",
        "advice.detachedHead=false",
        "clone",
        "--depth",
        "1",
        "--branch",
        tag,
    ])
    .arg("--")
    .arg(&url)
    .arg(&tree)
    .env("GIT_TERMINAL_PROMPT", "0");
    if root {
        // Root's own git configuration is not this job's business, and none
        // of it may redirect where the source comes from.
        git.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null");
    }
    checked(git.status()?, "git")?;

    say(&format!(
        "(2/3) Updating {repo}: cargo build --release --locked, which takes a while"
    ));
    let mut cargo = tool("cargo")?;
    cargo
        .args(["build", "--release", "--locked"])
        .current_dir(&tree)
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_TERM_COLOR", "never");
    if root {
        // A registry root keeps between builds, so the next one downloads
        // only what changed; never /root's, which is not this job's either.
        cargo.env("CARGO_HOME", Path::new(CACHE).join("cargo"));
    }
    checked(cargo.status()?, "cargo")?;

    say(&format!(
        "(3/3) Updating {repo}: installing {tag} under {}",
        prefix.display()
    ));
    let parts: Vec<Option<&str>> = if components.iter().all(String::is_empty) {
        vec![None]
    } else {
        components.iter().map(|c| Some(c.as_str())).collect()
    };
    for component in parts {
        let mut script = tool("bash")?;
        script
            .arg(tree.join("packaging/install.sh"))
            .arg("--destdir")
            .arg(&stage)
            .arg("--prefix")
            .arg(prefix)
            .current_dir(&tree)
            // Every tag's script reads this; not every tag's knows the flag.
            .env("CARGO_TARGET_DIR", &target);
        if let Some(component) = component {
            script.args(["--component", component]);
        }
        if let Some(libdir) = libdir {
            script.args(["--libdir", libdir]);
        }
        if let (Some(sitedir), Some("python")) = (sitedir, component) {
            script.args(["--python-sitedir", sitedir]);
        }
        checked(script.status()?, "install.sh")?;
    }
    // As root, only a record nobody but root could have written: it names
    // files to delete.
    let record = releases::record_path(prefix, repo);
    let previous = if root && process::trusted(record.clone()).is_err() {
        None
    } else {
        releases::record(prefix, repo)
    };
    let files = merge(&stage, prefix, root)?;
    if let Some(previous) = previous {
        let kept: BTreeSet<&PathBuf> = files.iter().collect();
        for old in previous.files.iter().filter(|f| !kept.contains(f)) {
            // Only what this installed, only under its own prefix, and only
            // files: a directory may hold anybody's.
            if releases::plain_path(old)
                && old.starts_with(prefix)
                && fs::symlink_metadata(old).is_ok_and(|m| !m.is_dir())
            {
                say(&format!(
                    "Removing {}, which {repo} {tag} no longer ships",
                    old.display()
                ));
                let _ = fs::remove_file(old);
            }
        }
    }
    fs::create_dir_all(record.parent().context("Invalid record path")?)?;
    replace(
        &record,
        &serde_json::to_vec_pretty(&Record {
            tag: tag.to_owned(),
            files,
        })?,
        0o644,
    )?;
    say(&format!(
        "{repo} {tag} is installed. Open programs keep the old version until they are restarted."
    ));
    Ok(())
}

fn checked(status: ExitStatus, what: &str) -> Result<()> {
    if !status.success() {
        bail!("{what} exited {}", status.code().unwrap_or(-1));
    }
    Ok(())
}

/// Put what `install.sh` staged onto the machine.
///
/// A copy of its own rather than the script run with `--destdir /`, for two
/// reasons. Everything is checked to land under the prefix (or, as root, in
/// `/etc`) before anything is written, so a script that staged something
/// elsewhere changes nothing. And a configuration file the administrator has
/// changed is kept: the new one is written beside it as `.lxbnew`, which is
/// what a package manager does with one it was told is configuration.
fn merge(stage: &Path, prefix: &Path, root: bool) -> Result<Vec<PathBuf>> {
    let mut entries = vec![];
    walk(stage, &mut entries)?;
    for entry in &entries {
        let destination = Path::new("/").join(entry.strip_prefix(stage)?);
        let directory = fs::symlink_metadata(entry)?.is_dir();
        let allowed = destination.starts_with(prefix)
            || (directory && prefix.starts_with(&destination))
            || (root && destination.starts_with("/etc"));
        if !allowed {
            bail!(
                "The install script staged {}, which is outside {}; nothing was installed",
                destination.display(),
                prefix.display()
            );
        }
    }
    let mut installed = vec![];
    for entry in &entries {
        let destination = Path::new("/").join(entry.strip_prefix(stage)?);
        let meta = fs::symlink_metadata(entry)?;
        if meta.is_dir() {
            // Followed: /usr/lib64 is a link to lib on a merged Arch, and a
            // directory reached through one is the directory.
            match fs::metadata(&destination) {
                Ok(existing) if existing.is_dir() => {}
                Ok(_) => bail!("{} is in the way of a directory", destination.display()),
                Err(_) => fs::DirBuilder::new()
                    .mode(meta.mode() & 0o7777)
                    .create(&destination)?,
            }
            continue;
        }
        if meta.file_type().is_symlink() {
            let target = fs::read_link(entry)?;
            let temporary = beside(&destination);
            let _ = fs::remove_file(&temporary);
            std::os::unix::fs::symlink(&target, &temporary)?;
            fs::rename(&temporary, &destination)?;
            installed.push(destination);
            continue;
        }
        let bytes = fs::read(entry)?;
        if destination.starts_with("/etc") && destination.exists() {
            if fs::read(&destination).ok().as_deref() != Some(&bytes[..]) {
                let new = PathBuf::from(format!("{}.lxbnew", destination.display()));
                replace(&new, &bytes, meta.mode() & 0o7777)?;
                say(&format!(
                    "Kept your {}; the version this release ships is beside it as {}",
                    destination.display(),
                    new.display()
                ));
            }
            installed.push(destination);
            continue;
        }
        replace(&destination, &bytes, meta.mode() & 0o7777)?;
        installed.push(destination);
    }
    Ok(installed)
}

fn walk(directory: &Path, into: &mut Vec<PathBuf>) -> Result<()> {
    let mut children: Vec<PathBuf> = fs::read_dir(directory)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<_>>()?;
    children.sort();
    for child in children {
        into.push(child.clone());
        if fs::symlink_metadata(&child)?.is_dir() {
            walk(&child, into)?;
        }
    }
    Ok(())
}

fn beside(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.lxb-updates"))
}

/// Write `bytes` to `path` by renaming a finished copy over it, so a running
/// program keeps the file it opened and nothing ever sees half of one.
fn replace(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let temporary = beside(path);
    let _ = fs::remove_file(&temporary);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)
        .with_context(|| format!("Could not install {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lxb-release-{name}-{}-{}",
            std::process::id(),
            crate::now()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    /// A merge into a prefix under a scratch directory, the way `/usr/local`
    /// would be merged into `/`: everything the script staged goes on, and
    /// a second release takes away what the first shipped and it does not.
    #[test]
    fn a_staged_tree_is_merged_and_nothing_outside_the_prefix_is_written() {
        let root = scratch("merge");
        let prefix = root.join("prefix");
        let stage = root.join("stage");
        let staged_prefix = stage.join(prefix.strip_prefix("/").unwrap());
        fs::create_dir_all(staged_prefix.join("bin")).unwrap();
        fs::write(staged_prefix.join("bin/songonsole"), "new").unwrap();
        fs::set_permissions(
            staged_prefix.join("bin/songonsole"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::os::unix::fs::symlink("songonsole", staged_prefix.join("bin/sos")).unwrap();
        fs::create_dir_all(prefix.join("bin")).unwrap();
        fs::write(prefix.join("bin/songonsole"), "old").unwrap();
        let files = merge(&stage, &prefix, false).unwrap();
        assert_eq!(
            fs::read_to_string(prefix.join("bin/songonsole")).unwrap(),
            "new"
        );
        assert_eq!(
            fs::metadata(prefix.join("bin/songonsole")).unwrap().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::read_link(prefix.join("bin/sos")).unwrap(),
            Path::new("songonsole")
        );
        assert_eq!(
            files,
            [prefix.join("bin/songonsole"), prefix.join("bin/sos")]
        );

        let elsewhere = stage
            .join(root.strip_prefix("/").unwrap())
            .join("elsewhere");
        fs::write(&elsewhere, "stray").unwrap();
        assert!(merge(&stage, &prefix, false).is_err());
        assert!(
            !root.join("elsewhere").exists(),
            "nothing is written when anything is out of bounds"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_fresh_directory_is_this_accounts_alone() {
        let parent = scratch("fresh");
        let one = fresh(&parent, 0o700).unwrap();
        assert_eq!(fs::metadata(&one).unwrap().mode() & 0o777, 0o700);
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            fresh(&parent, 0o700).is_err(),
            "a directory others can write into is not a place to build"
        );
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
        fs::remove_dir_all(parent).unwrap();
    }

    #[test]
    fn nothing_arrives_without_its_published_checksum() {
        let directory = scratch("digest");
        let package = |url: &str, digest: Option<&str>| releases::Package {
            file: "x.deb".into(),
            name: "x".into(),
            version: "1-1".into(),
            url: url.into(),
            size: 1,
            digest: digest.map(Into::into),
        };
        let good_url = "https://github.com/Petexy/CEDM/releases/download/v0.9.2/x.deb";
        let digest = format!("sha256:{}", "0".repeat(64));
        for (url, digest, why) in [
            (good_url, None, "no checksum"),
            (good_url, Some("md5:abc"), "not a SHA-256"),
            (
                "https://example.org/x.deb",
                Some(digest.as_str()),
                "not GitHub",
            ),
            (
                "https://github.com/Other/CEDM/releases/download/v0.9.2/x.deb",
                Some(digest.as_str()),
                "another account",
            ),
            (
                "https://github.com/Petexy/CEDM/releases/download/v0.9.2/../../x.deb",
                Some(digest.as_str()),
                "a path out of the release",
            ),
        ] {
            let error = download(
                "CEDM",
                "v0.9.2",
                &package(url, digest),
                &directory.join("x.deb"),
            );
            assert!(error.is_err(), "{why}");
            assert!(!directory.join("x.deb").exists(), "{why}");
        }
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn an_operation_runs_only_as_the_account_it_is_for() {
        let packages = serde_json::to_string(&Operation::Packages {
            targets: vec![Target {
                repo: "CEDM".into(),
                tag: "v0.9.2".into(),
                packages: vec!["cedm".into()],
            }],
        })
        .unwrap();
        if unsafe { libc::geteuid() } != 0 {
            assert!(run(&packages)
                .unwrap_err()
                .to_string()
                .contains("authorized update job"));
        }
        assert!(run(
            r#"{"kind":"packages","targets":[{"repo":"CEDM","tag":"v1","packages":["sh"]}]}"#
        )
        .is_err());
        assert!(run("reboot").is_err());
    }
}
