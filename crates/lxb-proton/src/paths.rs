//! Where this integration keeps things, and why the two places are two.
//!
//! **The store** — `$XDG_DATA_HOME/lxb/proton`, or `~/.local/share/lxb/proton`
//! where that is unset — holds the managed Proton and the runtimes. They are
//! shared: one copy serves every Windows game, and taking a game off the disk
//! must not take the Proton with it.
//!
//! **A prefix root** holds one directory per game, each of which is that
//! game's whole Windows installation — its files, its registry, and the saves
//! it keeps beside itself. `~/.lxb-proton` by default.
//!
//! The default is a dot-directory in `$HOME` rather than somewhere under
//! `$XDG_DATA_HOME`, which is what the rest of this project would do, and the
//! reason is the thing itself: a Windows game is tens of gigabytes, people
//! move them between disks, and a path somebody has to be able to find, name
//! and copy is worth more here than one more directory nobody ever looks in.
//! Which is also why the folder can be changed — see [`Paths::roots`].

use std::path::{Path, PathBuf};

/// Where everything is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    home: PathBuf,
    store: PathBuf,
    /// Every root prefixes have been made under, the one new ones go in
    /// first.
    roots: Vec<PathBuf>,
}

/// What the shell writes to say the folder has been changed, inside the
/// store: one path per line, the first being where new prefixes go.
///
/// A file rather than a setting read out of the shell's own configuration,
/// because the helper must answer `probe` on a machine where the shell is
/// not running — and because the *old* roots have to outlive the setting.
/// Somebody who moves the folder still owns the games they installed before
/// they moved it, and a list that only remembered the current answer would
/// lose them.
const ROOTS: &str = "prefix-roots";

impl Paths {
    /// This user's, or `None` where there is no home directory to put them
    /// in.
    pub fn of_this_user() -> Option<Paths> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())?;
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|at| at.is_absolute())
            .unwrap_or_else(|| home.join(".local/share"));
        Some(Paths::at(&home, &data))
    }

    /// The same, below a given home and data directory.
    ///
    /// Both are arguments rather than read from the environment here, so a
    /// test can put the whole arrangement somewhere harmless — a `Paths`
    /// that read `XDG_DATA_HOME` itself would have every test of this file
    /// writing into the real one.
    pub fn at(home: &Path, data_home: &Path) -> Paths {
        let mut paths = Paths {
            home: home.to_path_buf(),
            store: data_home.join("lxb/proton"),
            roots: Vec::new(),
        };
        paths.roots = paths.read_roots();
        paths
    }

    /// The shared Proton and runtime store.
    pub fn store(&self) -> &Path {
        &self.store
    }

    /// Where the managed Protons are unpacked.
    pub fn protons(&self) -> PathBuf {
        self.store.join("protons")
    }

    /// Where the managed runtimes are unpacked, one directory per
    /// generation.
    pub fn runtimes(&self) -> PathBuf {
        self.store.join("runtimes")
    }

    /// Somewhere to put a download while it is being checked. Beside where
    /// it will live, so moving it into place is a rename rather than a copy
    /// across a filesystem.
    pub fn downloads(&self) -> PathBuf {
        self.store.join("downloads")
    }

    /// Every root prefixes may be under, the one new ones go in first.
    ///
    /// Always at least one. A root that is not there today is still listed:
    /// the games on an unplugged disk are not gone, and a column that
    /// quietly dropped them would be telling somebody their games had been
    /// deleted.
    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// Where a new prefix goes.
    pub fn new_prefixes(&self) -> &Path {
        &self.roots[0]
    }

    /// The default, which is what a machine that has never been told
    /// otherwise uses.
    pub fn default_root(&self) -> PathBuf {
        self.home.join(".lxb-proton")
    }

    fn read_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = std::fs::read_to_string(self.store.join(ROOTS))
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(PathBuf::from)
            .filter(|at| at.is_absolute())
            .collect();
        let default = self.default_root();
        // The default is always in the list, because a game installed before
        // anybody changed the setting is under it — but only behind whatever
        // the setting says now, so a changed setting takes effect.
        if !roots.contains(&default) {
            roots.push(default);
        }
        roots
    }

    /// Put new prefixes here from now on, keeping every root already used so
    /// the games under them are still found.
    pub fn use_root(&mut self, at: &Path) -> std::io::Result<()> {
        let at = at.to_path_buf();
        let mut roots = vec![at.clone()];
        roots.extend(self.roots.iter().filter(|had| **had != at).cloned());
        std::fs::create_dir_all(&self.store)?;
        let text: String = roots
            .iter()
            .map(|at| format!("{}\n", at.display()))
            .collect();
        let temp = self
            .store
            .join(format!("{ROOTS}.lxb-{}", std::process::id()));
        std::fs::write(&temp, text)?;
        std::fs::rename(&temp, self.store.join(ROOTS)).inspect_err(|_| {
            let _ = std::fs::remove_file(&temp);
        })?;
        self.roots = roots;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch arrangement that cannot reach outside itself.
    fn scratch(home: &Path) -> Paths {
        Paths::at(home, &home.join(".local/share"))
    }

    #[test]
    fn the_store_is_under_the_data_home_and_the_prefixes_are_not() {
        let home = tempfile::tempdir().unwrap();
        let paths = scratch(home.path());
        assert_eq!(
            paths.store(),
            home.path().join(".local/share/lxb/proton"),
            "the store is data, and a prefix is not"
        );
        assert_eq!(paths.protons(), paths.store().join("protons"));
        assert_eq!(paths.runtimes(), paths.store().join("runtimes"));
        assert_eq!(paths.default_root(), home.path().join(".lxb-proton"));
    }

    /// With nothing written, the default is the only root and is where new
    /// prefixes go.
    #[test]
    fn a_machine_nobody_has_moved_has_one_root() {
        let home = tempfile::tempdir().unwrap();
        let paths = scratch(home.path());
        assert_eq!(paths.roots(), [home.path().join(".lxb-proton")]);
        assert_eq!(paths.new_prefixes(), home.path().join(".lxb-proton"));
    }

    /// Moving the folder changes where new games go and keeps the old root,
    /// because the games already under it are still somebody's games. This
    /// is the whole reason the list is a list.
    #[test]
    fn moving_the_folder_keeps_the_games_already_installed() {
        let home = tempfile::tempdir().unwrap();
        let mut paths = scratch(home.path());
        let elsewhere = home.path().join("disk/Games");
        paths.use_root(&elsewhere).unwrap();

        assert_eq!(paths.new_prefixes(), elsewhere);
        assert!(paths.roots().contains(&home.path().join(".lxb-proton")));

        // And it is remembered, which is the point of writing it down.
        let read_again = scratch(home.path());
        assert_eq!(read_again.roots(), paths.roots());

        // Moving back puts the default in front again without a third entry.
        let mut paths = read_again;
        let default = paths.default_root();
        paths.use_root(&default).unwrap();
        assert_eq!(paths.new_prefixes(), default);
        assert_eq!(paths.roots().len(), 2, "{:?}", paths.roots());
    }
}
