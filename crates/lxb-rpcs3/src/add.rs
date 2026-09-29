//! Installing a package into RPCS3's PlayStation 3, so it can be played.
//!
//! **RPCS3's own installer does the installing** (`--headless --installpkg`):
//! it knows which updates fit which game version, where an add-on goes, and
//! what to do with a file it has seen before, and a copy of that here would be
//! a second opinion that could disagree. What this adds around it is the part
//! RPCS3 does not do:
//!
//! * **A zipped package is taken out of its zip first**, into a scratch folder
//!   RPCS3 can read — the flatpak's own cache, for a flatpak — and deleted once
//!   RPCS3 is done with it. The user's zip is not touched.
//! * **The licence goes where RPCS3 looks for it**: a `.rap` in the zip, or
//!   beside a loose package, is copied into the console user's `exdata`
//!   folder under its own name — which is where RPCS3's own window puts one
//!   dropped onto it, lower-case extension and all.
//! * **A bar.** RPCS3's headless installer says nothing while it works, so how
//!   far along it is is read off the disk: what has arrived in the game's
//!   folder, against what the package says it holds.
//! * **A game and its updates in one go.** Given several packages — a game
//!   and the updates beside it, in version order, as the scan groups them —
//!   they are installed one after another under one bar, and only the last
//!   one's end is the end.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::pkg::{Package, Source};
use crate::report::{Installation, Kind, Stage, Trouble};
use crate::rpcs3::{self, Console};
use crate::setup::tell;
use crate::sfo::Sfo;
use crate::zip;

/// One bar over several packages: each one's progress is its share of the
/// whole, and only the last one says it is done.
#[derive(Debug, Clone, Copy)]
struct Bar {
    at: usize,
    of: usize,
}

impl Bar {
    fn tell(
        self,
        out: &mut impl Write,
        stage: Stage,
        progress: Option<f32>,
        trouble: Option<Trouble>,
        note: &str,
    ) {
        let last = self.at + 1 >= self.of;
        let (stage, progress) = match stage {
            Stage::Done if !last => (Stage::Adding, Some(1.0)),
            _ => (stage, progress),
        };
        let share = |done: f32| (self.at as f32 + done) / self.of.max(1) as f32;
        tell(out, stage, progress.map(share), trouble, note);
    }
}

/// Install the packages at `paths`, in the order given — each a `.pkg`, or a
/// zip with one in it. A game and its updates, oldest first.
pub fn run(out: &mut impl Write, paths: &[PathBuf]) -> bool {
    for (at, path) in paths.iter().enumerate() {
        if !run_one(
            out,
            path,
            Bar {
                at,
                of: paths.len(),
            },
        ) {
            return false;
        }
    }
    true
}

/// Install the package at `path`, as `bar`'s share of the whole.
fn run_one(out: &mut impl Write, path: &Path, bar: Bar) -> bool {
    let Some(installation) = rpcs3::installation() else {
        bar.tell(out, Stage::Failed, None, Some(Trouble::NoRpcs3), "no RPCS3");
        return false;
    };
    let (Some(config), Some(scratch)) = (
        rpcs3::config_dir(&installation),
        rpcs3::scratch_dir(&installation),
    ) else {
        bar.tell(
            out,
            Stage::Failed,
            None,
            Some(Trouble::Other),
            "no home directory",
        );
        return false;
    };
    let console = Console::of(&config);

    let zipped = path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"));
    let (package_file, licences, temporary) = if zipped {
        match unzip(out, path, &scratch, bar) {
            Ok((package_file, licences)) => (package_file, licences, true),
            Err((trouble, note)) => {
                bar.tell(out, Stage::Failed, None, Some(trouble), &note);
                return false;
            }
        }
    } else if readable_by(&installation, path) {
        (
            path.to_path_buf(),
            Licences::Beside(path.to_path_buf()),
            false,
        )
    } else {
        // A loose package somewhere a flatpak cannot see is copied where it
        // can, rather than widening its sandbox for one file.
        match copy_in(out, path, &scratch, bar) {
            Ok(copy) => (copy, Licences::Beside(path.to_path_buf()), true),
            Err((trouble, note)) => {
                bar.tell(out, Stage::Failed, None, Some(trouble), &note);
                return false;
            }
        }
    };

    let done = install(out, &installation, &console, &package_file, &licences, bar);
    if temporary {
        let _ = std::fs::remove_file(&package_file);
    }
    done
}

/// Where a package's licence is: the `.rap` files that came out of its zip,
/// or the folder a loose package sits in.
enum Licences {
    Unzipped(Vec<(String, Vec<u8>)>),
    Beside(PathBuf),
}

fn install(
    out: &mut impl Write,
    installation: &Installation,
    console: &Console,
    package_file: &Path,
    licences: &Licences,
    bar: Bar,
) -> bool {
    let package = match Source::file(package_file).and_then(Package::open) {
        Ok(package) => package,
        Err(err) => {
            bar.tell(
                out,
                Stage::Failed,
                None,
                Some(Trouble::NotAPackage),
                &err.to_string(),
            );
            return false;
        }
    };
    let content_id = package.content_id.clone();
    let target = console.games().join(&package.install_dir);
    let expected = package.unpacked_size().max(1);
    let wanted_version = {
        let mut package = package;
        package
            .item("PARAM.SFO")
            .cloned()
            .and_then(|item| package.read(&item, 1 << 20).ok().flatten())
            .and_then(|bytes| Sfo::parse(&bytes))
            .and_then(|sfo| sfo.app_version().map(str::to_string))
    };

    if let Err(err) = place_licences(console, &content_id, licences) {
        eprintln!("add: the licence could not be put in place: {err}");
    }

    bar.tell(
        out,
        Stage::Adding,
        Some(0.0),
        None,
        "RPCS3 is installing the package",
    );
    let before = folder_size(&target);
    let (program, rest) = match installation.command.split_first() {
        Some(split) => split,
        None => {
            bar.tell(
                out,
                Stage::Failed,
                None,
                Some(Trouble::NoRpcs3),
                "no command",
            );
            return false;
        }
    };
    let child = std::process::Command::new(program)
        .args(rest)
        .args(["--headless", "--installpkg"])
        .arg(package_file)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .env("QT_QPA_PLATFORM", "offscreen")
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(err) => {
            bar.tell(
                out,
                Stage::Failed,
                None,
                Some(Trouble::NoRpcs3),
                &err.to_string(),
            );
            return false;
        }
    };
    let mut said = 0u32;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(err) => {
                bar.tell(
                    out,
                    Stage::Failed,
                    None,
                    Some(Trouble::Other),
                    &err.to_string(),
                );
                return false;
            }
        }
        let arrived = folder_size(&target).saturating_sub(before);
        let fraction = (arrived as f64 / expected as f64).min(0.99) as f32;
        let percent = (fraction * 100.0) as u32;
        if percent > said {
            said = percent;
            bar.tell(
                out,
                Stage::Adding,
                Some(fraction),
                None,
                "RPCS3 is installing the package",
            );
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    // RPCS3's headless installer answers 0 whatever happened: what it did is
    // read off the disk.
    let installed = std::fs::read(target.join("PARAM.SFO"))
        .ok()
        .and_then(|bytes| Sfo::parse(&bytes));
    let fits = installed.as_ref().is_some_and(|sfo| match &wanted_version {
        Some(version) => sfo.app_version() >= Some(version.as_str()),
        None => true,
    });
    if fits {
        bar.tell(
            out,
            Stage::Done,
            Some(1.0),
            None,
            &format!("installed into {}", target.display()),
        );
        true
    } else {
        bar.tell(
            out,
            Stage::Failed,
            None,
            Some(Trouble::PackageRejected),
            "RPCS3 did not install the package",
        );
        false
    }
}

/// Take the package out of its zip, and keep the licences that came with it.
fn unzip(
    out: &mut impl Write,
    archive: &Path,
    scratch: &Path,
    bar: Bar,
) -> Result<(PathBuf, Licences), (Trouble, String)> {
    let entries = zip::entries(archive).map_err(|err| (Trouble::NotAPackage, err.to_string()))?;
    let packaged = entries
        .iter()
        .find(|entry| !entry.is_directory() && entry.file_name().to_lowercase().ends_with(".pkg"))
        .ok_or_else(|| {
            (
                Trouble::NotAPackage,
                "there is no package in the zip".to_string(),
            )
        })?;
    let mut licences = Vec::new();
    for entry in &entries {
        if !entry.file_name().to_lowercase().ends_with(".rap") || entry.size > 64 {
            continue;
        }
        let mut reader = zip::EntryReader::open(archive, entry)
            .map_err(|err| (Trouble::NotAPackage, err.to_string()))?;
        let mut bytes = Vec::new();
        reader
            .stream()
            .read_to_end(&mut bytes)
            .map_err(|err| (Trouble::NotAPackage, err.to_string()))?;
        licences.push((entry.file_name().to_string(), bytes));
    }

    std::fs::create_dir_all(scratch).map_err(|err| (crate::net::disk(&err), err.to_string()))?;
    let at = scratch.join(packaged.file_name());
    let mut reader = zip::EntryReader::open(archive, packaged)
        .map_err(|err| (Trouble::NotAPackage, err.to_string()))?;
    bar.tell(
        out,
        Stage::Unpacking,
        Some(0.0),
        None,
        "taking the package out of its zip",
    );
    let total = packaged.size.max(1);
    let result = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&at)?;
        let mut crc = flate2::Crc::new();
        let mut buffer = vec![0u8; 1 << 20];
        let mut done = 0u64;
        let mut said = 0u32;
        loop {
            let read = reader.stream().read(&mut buffer)?;
            if read == 0 {
                break;
            }
            crc.update(&buffer[..read]);
            file.write_all(&buffer[..read])?;
            done += read as u64;
            let percent = (done * 100 / total) as u32;
            if percent > said {
                said = percent;
                bar.tell(
                    out,
                    Stage::Unpacking,
                    Some(done as f32 / total as f32),
                    None,
                    "taking the package out of its zip",
                );
            }
        }
        if done != packaged.size || crc.sum() != packaged.crc {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "the zip is damaged",
            ));
        }
        file.sync_all()
    })();
    if let Err(err) = result {
        let _ = std::fs::remove_file(&at);
        let trouble = match crate::net::disk(&err) {
            Trouble::NoSpace => Trouble::NoSpace,
            _ => Trouble::NotAPackage,
        };
        return Err((trouble, err.to_string()));
    }
    Ok((at, Licences::Unzipped(licences)))
}

/// A loose package copied where the sandbox can read it.
fn copy_in(
    out: &mut impl Write,
    path: &Path,
    scratch: &Path,
    bar: Bar,
) -> Result<PathBuf, (Trouble, String)> {
    std::fs::create_dir_all(scratch).map_err(|err| (crate::net::disk(&err), err.to_string()))?;
    let at = scratch.join(path.file_name().unwrap_or_default());
    bar.tell(out, Stage::Unpacking, None, None, "copying the package");
    std::fs::copy(path, &at).map_err(|err| (crate::net::disk(&err), err.to_string()))?;
    Ok(at)
}

/// The licences into `exdata`, each under `<content id>.rap`.
fn place_licences(console: &Console, content_id: &str, licences: &Licences) -> std::io::Result<()> {
    let found: Vec<(String, Vec<u8>)> = match licences {
        Licences::Unzipped(found) => found.clone(),
        Licences::Beside(package) => {
            let mut found = Vec::new();
            if let Some(dir) = package.parent() {
                for entry in std::fs::read_dir(dir)?.filter_map(Result::ok) {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.to_lowercase().ends_with(".rap") && name.starts_with(content_id) {
                        found.push((name, std::fs::read(entry.path())?));
                    }
                }
            }
            found
        }
    };
    let folder = console.licences();
    for (name, bytes) in found {
        if bytes.len() != 16 {
            continue;
        }
        std::fs::create_dir_all(&folder)?;
        // RPCS3 finds a licence only under a lower-case extension.
        let stem = name
            .rsplit_once('.')
            .map_or(name.as_str(), |(stem, _)| stem);
        let at = folder.join(format!("{stem}.rap"));
        std::fs::write(&at, &bytes)?;
        eprintln!("add: licence {}", at.display());
    }
    Ok(())
}

/// Whether RPCS3 can open this file where it is.
fn readable_by(installation: &Installation, path: &Path) -> bool {
    match installation.kind {
        Kind::System | Kind::AppImage => true,
        Kind::FlatpakUser | Kind::FlatpakSystem => {
            path.starts_with("/media")
                || path.starts_with("/run/media")
                || rpcs3::home().is_some_and(|home| path.starts_with(home))
        }
    }
}

/// The bytes in a folder and everything under it.
fn folder_size(at: &Path) -> u64 {
    let Ok(listing) = std::fs::read_dir(at) else {
        return 0;
    };
    listing
        .filter_map(Result::ok)
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => folder_size(&entry.path()),
            Ok(_) => entry.metadata().map(|meta| meta.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn licences_land_under_the_content_id_with_a_lower_case_extension() {
        let dir = tempfile::tempdir().unwrap();
        let console = Console::of(dir.path());
        let content = "EP9000-NPEA00019_00-TEKKENRETAIL0000";
        let licences = Licences::Unzipped(vec![(format!("{content}.RAP"), vec![7; 16])]);
        place_licences(&console, content, &licences).unwrap();
        let placed = console.licences().join(format!("{content}.rap"));
        assert_eq!(std::fs::read(placed).unwrap(), vec![7; 16]);
    }

    #[test]
    fn a_zipped_package_comes_out_whole() {
        let dir = tempfile::tempdir().unwrap();
        let content = "EP9000-NPEA00019_00-TEKKENRETAIL0000";
        let package = crate::pkg::build(content, 0x05, &[("PARAM.SFO", b"\0PSF")]);
        let archive = dir.path().join("t5.zip");
        std::fs::write(
            &archive,
            zip::build(
                &[("x.pkg", &package), (&format!("{content}.rap"), &[3; 16])],
                true,
            ),
        )
        .unwrap();
        let mut said = Vec::new();
        let (unpacked, licences) = unzip(
            &mut said,
            &archive,
            &dir.path().join("scratch"),
            Bar { at: 0, of: 1 },
        )
        .map_err(|(_, note)| note)
        .unwrap();
        assert_eq!(std::fs::read(&unpacked).unwrap(), package);
        let Licences::Unzipped(found) = licences else {
            panic!("the licence came out of the zip");
        };
        assert_eq!(found, vec![(format!("{content}.rap"), vec![3; 16])]);
        assert!(String::from_utf8(said).unwrap().contains("\"unpacking\""));
    }

    /// Two packages are one bar: the first one's end is halfway, and only the
    /// second one's is the end.
    #[test]
    fn a_game_and_its_update_are_one_bar() {
        let mut said = Vec::new();
        let first = Bar { at: 0, of: 2 };
        first.tell(&mut said, Stage::Adding, Some(0.5), None, "");
        first.tell(&mut said, Stage::Done, Some(1.0), None, "");
        let second = Bar { at: 1, of: 2 };
        second.tell(&mut said, Stage::Adding, Some(0.5), None, "");
        second.tell(&mut said, Stage::Done, Some(1.0), None, "");
        let lines: Vec<crate::report::Progress> = String::from_utf8(said)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let seen: Vec<(Stage, Option<f32>)> = lines
            .iter()
            .map(|line| (line.stage, line.progress))
            .collect();
        assert_eq!(
            seen,
            [
                (Stage::Adding, Some(0.25)),
                (Stage::Adding, Some(0.5)),
                (Stage::Adding, Some(0.75)),
                (Stage::Done, Some(1.0)),
            ]
        );
    }
}
