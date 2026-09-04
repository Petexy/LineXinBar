//! Which game a window's process belongs to.
//!
//! A window says nothing about who started it. What it announces is a class —
//! the name whoever packaged the program chose — and for a game under Proton
//! that is whatever the binary was called, routinely `x86_64` for a whole shelf
//! of them. Nothing in the window, at any point, says "I am the game somebody
//! pressed A on two seconds ago".
//!
//! So the shell matched by *timing*: while a loading screen is up, the window
//! that was not there before is the game. That is right until two things appear
//! at once, which is not rare — Valve's client raises its own windows on its own
//! schedule, an anti-cheat installer comes and goes, a launcher is swapped for
//! the game itself, and anything else on the machine is free to open a window
//! while a game is loading. Every one of those was recorded as the game, and
//! from then on the guide offered to close the game and closed something else.
//!
//! There is one thing that does say so, and Steam puts it there itself: a game
//! it starts is run with `SteamAppId` and `SteamGameId` in its environment. That
//! is the app id, from the program that assigned it, on the process that drew
//! the window — not a guess about a name. This module reads it.
//!
//! **It is read, never trusted to exist.** A great many windows have no such
//! variable and are perfectly ordinary; a process may be gone by the time it is
//! asked, or belong to another user and not be readable at all. Every `None`
//! here means "this says nothing", and the caller falls back to the timing it
//! always had. What the answer is *for* is the other direction: a window that
//! says it is a different game, or says nothing while something else already
//! claimed it, is one the shell can stop attributing by accident.
//!
//! ## And who is on the other end of a port
//!
//! The second half of this module answers a different question out of the same
//! `/proc`: *which process is listening on `127.0.0.1:8080`*, and is it one of
//! the family of the client this session proved. A generation check says the
//! account has not moved and says nothing about the socket the call goes out
//! on — see [`crate::webui::Still`], which is where the two are put together.

use std::path::Path;

/// How far up the family to look.
///
/// The variable is inherited, so the game's own process has it and so does
/// anything it starts. What this is for is the other way round: a game whose
/// window is drawn by a child that was started through something which cleared
/// its environment — a wrapper, a sandbox helper, a launcher re-execing itself
/// — where the parent still has it. Eight is well past every real chain and
/// short enough that a session cannot spend its time walking `/proc`.
///
/// [`one_family`] walks the same distance for the same reason: between Valve's
/// client and the `steamwebhelper` holding its debugging port there is a
/// reaper and, on some machines, a runtime wrapper.
const ANCESTORS: usize = 8;

/// What Steam stamps on a game it starts.
///
/// Both, and in this order. `SteamAppId` is the app id and is what a shell
/// wants; `SteamGameId` is the same number for a game out of the library and a
/// far larger one for a shortcut somebody added themselves, which is not an app
/// id and does not fit in one — so it fails to parse and is passed over, which
/// is the correct answer rather than a lucky one.
const SAYS_WHICH_GAME: [&str; 2] = ["SteamAppId", "SteamGameId"];

/// The game one process belongs to, if it says so.
pub fn game_of(pid: u32) -> Option<u32> {
    game_under(Path::new("/proc"), pid)
}

/// The same, below a directory standing in for `/proc`, so a test can build a
/// process table of its own rather than asking about this machine's.
pub(crate) fn game_under(proc: &Path, pid: u32) -> Option<u32> {
    let mut pid = pid;
    for _ in 0..ANCESTORS {
        if let Some(game) = stamped_on(proc, pid) {
            return Some(game);
        }
        // `1` is init and `0` is the answer for a process that has gone; a
        // parent that is its own child is not a thing the kernel produces, and
        // is checked because a fabricated `/proc` in a test is.
        match parent_of(proc, pid) {
            Some(parent) if parent > 1 && parent != pid => pid = parent,
            _ => return None,
        }
    }
    None
}

/// The app id in one process's own environment.
fn stamped_on(proc: &Path, pid: u32) -> Option<u32> {
    let environ = std::fs::read(proc.join(pid.to_string()).join("environ")).ok()?;
    // Null-separated, and not necessarily valid UTF-8 as a whole: one unreadable
    // variable must not lose the rest.
    environ.split(|byte| *byte == 0).find_map(|entry| {
        let (name, value) = std::str::from_utf8(entry).ok()?.split_once('=')?;
        if !SAYS_WHICH_GAME.contains(&name) {
            return None;
        }
        // Zero is what `SteamAppId` says for something that is not a game out
        // of the library at all, and it is not an app id.
        value.parse::<u32>().ok().filter(|app_id| *app_id != 0)
    })
}

/// Who started it. From `status` rather than `stat`, whose second field is a
/// program name that may itself contain spaces and brackets.
fn parent_of(proc: &Path, pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(proc.join(pid.to_string()).join("status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:")?.trim().parse::<u32>().ok())
}

/// Whether two pids are the same process or two of one family.
///
/// Both directions, deliberately. What this answers is "is the program on the
/// other end of that port part of the client I proved", and neither side of
/// that is reliably the parent: Valve's client starts `steamwebhelper`, which
/// is what actually listens, but a client that re-execs itself on the way up
/// leaves the process holding the pipe *below* the one that opened the port.
/// Asking one way round would refuse a perfectly ordinary client.
pub(crate) fn one_family(pid: u32, other: u32) -> bool {
    one_family_under(Path::new("/proc"), pid, other)
}

/// The same, below a directory standing in for `/proc`.
pub(crate) fn one_family_under(proc: &Path, pid: u32, other: u32) -> bool {
    pid == other || descends_from(proc, pid, other) || descends_from(proc, other, pid)
}

/// Whether `pid` is `ancestor`, or was started by it, within [`ANCESTORS`].
fn descends_from(proc: &Path, pid: u32, ancestor: u32) -> bool {
    let mut pid = pid;
    for _ in 0..ANCESTORS {
        if pid == ancestor {
            return true;
        }
        match parent_of(proc, pid) {
            // The same three guards [`game_under`] walks under, and for the
            // same reason: `1` is init, `0` is a process that has gone, and a
            // process that is its own parent only exists in a fabricated table.
            Some(parent) if parent > 1 && parent != pid => pid = parent,
            _ => return false,
        }
    }
    false
}

/// What the second field of a `/proc/net/tcp` row says about a socket that is
/// listening.
const LISTENING: &str = "0A";

/// Which processes are listening on one loopback port.
///
/// Empty means "could not find out", which is not the same as "nobody": a row
/// whose owner belongs to another user cannot be read, and a socket can close
/// between the table being read and `/proc` being walked. The caller is the one
/// that knows what an unknown answer is worth — see
/// [`crate::webui::Still`], which treats it as no evidence rather than as a
/// refusal.
pub(crate) fn listening_on(port: u16) -> Vec<u32> {
    listening_under(Path::new("/proc"), port)
}

/// The same, below a directory standing in for `/proc`.
pub(crate) fn listening_under(proc: &Path, port: u16) -> Vec<u32> {
    let sockets = listening_sockets(proc, port);
    if sockets.is_empty() {
        return Vec::new();
    }
    holders_of(proc, &sockets)
}

/// The inodes of every socket listening on that port, over loopback.
///
/// Both tables, because whether the client's Chromium binds `127.0.0.1` or
/// `::1` is Chromium's business and not this shell's.
fn listening_sockets(proc: &Path, port: u16) -> Vec<u64> {
    ["net/tcp", "net/tcp6"]
        .iter()
        .filter_map(|table| std::fs::read_to_string(proc.join(table)).ok())
        .flat_map(|table| {
            table
                .lines()
                .filter_map(|row| listening_row(row, port))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// One row of that table, if it is a socket listening on this port and a
/// connection to loopback could land on it.
fn listening_row(row: &str, port: u16) -> Option<u64> {
    let mut fields = row.split_whitespace();
    let _sl = fields.next()?;
    let local = fields.next()?;
    let _remote = fields.next()?;
    if fields.next()? != LISTENING {
        return None;
    }
    // What is left, in order: the queues, the timer, the retransmits, the uid,
    // the timeout, and then the inode.
    let inode = fields.nth(5)?.parse::<u64>().ok()?;
    let (address, on) = local.rsplit_once(':')?;
    if u16::from_str_radix(on, 16).ok()? != port {
        return None;
    }
    reachable_over_loopback(address).then_some(inode)
}

/// Whether an address out of that table is one a connection to `127.0.0.1`
/// could arrive on.
///
/// The addresses are written as the words the kernel holds them in, in hex, so
/// `127.0.0.1` is the little-endian `0100007F` rather than anything that reads
/// like an address. Three shapes matter and the rest are somebody else's
/// interface: the wildcard, loopback itself, and — for the second table — a
/// v4 address mapped into v6, which is what a socket bound to `0.0.0.0` on a
/// dual-stack machine appears as.
fn reachable_over_loopback(address: &str) -> bool {
    /// A v4 address bound to the whole loopback network: the low byte of the
    /// little-endian word is the leading `127`.
    fn loopback_v4(word: &str) -> bool {
        word.len() == 8 && word.ends_with("7F")
    }
    let all_zero = |word: &str| word.chars().all(|digit| digit == '0');
    match address.len() {
        8 => all_zero(address) || loopback_v4(address),
        32 => {
            all_zero(address)
                || address == "00000000000000000000000001000000"
                || address
                    .strip_prefix("0000000000000000FFFF0000")
                    .is_some_and(|v4| all_zero(v4) || loopback_v4(v4))
        }
        _ => false,
    }
}

/// Which processes hold any of those sockets open.
///
/// One walk of `/proc` for all of them rather than one apiece: the walk is the
/// expensive half, and this is asked on the way to an install.
fn holders_of(proc: &Path, sockets: &[u64]) -> Vec<u32> {
    let wanted = sockets
        .iter()
        .map(|inode| format!("socket:[{inode}]"))
        .collect::<Vec<_>>();
    let mut holders = Vec::new();
    let Ok(processes) = std::fs::read_dir(proc) else {
        return holders;
    };
    for process in processes.flatten() {
        let Some(pid) = process
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        // Unreadable for a process belonging to another user, which is exactly
        // the case this answers "could not find out" for.
        let Ok(open) = std::fs::read_dir(process.path().join("fd")) else {
            continue;
        };
        for handle in open.flatten() {
            let Ok(target) = std::fs::read_link(handle.path()) else {
                continue;
            };
            if target
                .to_str()
                .is_some_and(|target| wanted.iter().any(|socket| socket == target))
            {
                holders.push(pid);
                break;
            }
        }
    }
    holders
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A process table of this test's own.
    struct Fake(PathBuf);

    impl Fake {
        fn new(name: &str) -> Fake {
            let path = std::env::temp_dir().join(format!("lxb-proc-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("a scratch process table");
            Fake(path)
        }

        /// One process: its parent, and the environment it was started with.
        fn process(&self, pid: u32, parent: u32, environment: &[(&str, &str)]) -> &Fake {
            let dir = self.0.join(pid.to_string());
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("status"),
                format!("Name:\tsomething\nPPid:\t{parent}\n"),
            )
            .unwrap();
            let mut environ = Vec::new();
            for (name, value) in environment {
                environ.extend_from_slice(format!("{name}={value}").as_bytes());
                environ.push(0);
            }
            std::fs::write(dir.join("environ"), environ).unwrap();
            self
        }

        fn game_of(&self, pid: u32) -> Option<u32> {
            game_under(&self.0, pid)
        }

        /// One row of the kernel's TCP table, in the shape and the order the
        /// real one is written in.
        fn listening(&self, address: &str, port: u16, inode: u64) -> &Fake {
            let net = self.0.join("net");
            std::fs::create_dir_all(&net).unwrap();
            std::fs::write(
                net.join("tcp"),
                format!(
                    "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when                        retrnsmt   uid  timeout inode\n                          0: {address}:{port:04X} 00000000:0000 0A 00000000:00000000                        00:00000000 00000000  1000        0 {inode} 1 0 0 10 0\n"
                ),
            )
            .unwrap();
            self
        }

        /// One process holding one socket open, as `/proc` writes it: a link
        /// to a name no directory has.
        fn holds(&self, pid: u32, inode: u64) -> &Fake {
            let fd = self.0.join(pid.to_string()).join("fd");
            std::fs::create_dir_all(&fd).unwrap();
            std::os::unix::fs::symlink(format!("socket:[{inode}]"), fd.join("7")).unwrap();
            self
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The whole of the port half, against the kernel's own table rather than
    /// a fabricated one: a socket this test is holding open names this test's
    /// process.
    ///
    /// The one part of it no fake can stand in for. What it pins is that the
    /// inode in `/proc/net/tcp` is the inode in `/proc/<pid>/fd`, which is the
    /// step the whole answer rests on.
    #[test]
    fn a_socket_this_process_is_holding_names_this_process() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").expect("a port of our own");
        let port = held.local_addr().unwrap().port();
        assert!(
            listening_on(port).contains(&std::process::id()),
            "a listening socket did not name the process holding it"
        );
        // And a port nobody is on is nobody's, rather than somebody's by
        // accident: the listener goes and so does the answer.
        drop(held);
        assert!(listening_on(port).is_empty());
    }

    /// The client and the helper holding its debugging port are one family,
    /// whichever way round they were started.
    #[test]
    fn the_helper_that_holds_the_port_is_the_client() {
        let proc = Fake::new("family");
        // Valve's client, its reaper, and the web helper that listens.
        proc.process(900, 1, &[])
            .process(901, 900, &[])
            .process(902, 901, &[]);
        assert!(one_family_under(&proc.0, 902, 900));
        // Both ways round: a client that re-execs itself on the way up leaves
        // the process holding the pipe below the one that opened the port.
        assert!(one_family_under(&proc.0, 900, 902));
        assert!(one_family_under(&proc.0, 900, 900));

        // And a Steam somebody else started is not this one, however much it
        // looks like it.
        proc.process(800, 1, &[]).process(801, 800, &[]);
        assert!(!one_family_under(&proc.0, 801, 900));
    }

    /// A row of the table becomes the process holding it — and a row on an
    /// address a loopback connection could never arrive on is not read at all.
    #[test]
    fn only_a_socket_a_loopback_connection_could_reach_counts() {
        let proc = Fake::new("port");
        // `0100007F` is 127.0.0.1 as the kernel holds it: little-endian, and
        // nothing that reads like an address.
        proc.listening("0100007F", 8080, 4242).holds(900, 4242);
        assert_eq!(listening_under(&proc.0, 8080), vec![900]);
        // The same socket, on this machine's address on the network. Nothing
        // this shell connects to over loopback can land there, so it is not
        // the client's port and its owner is not the client.
        let elsewhere = Fake::new("elsewhere");
        elsewhere.listening("0F02000A", 8080, 4242).holds(900, 4242);
        assert!(listening_under(&elsewhere.0, 8080).is_empty());
        // Nor is another port's owner.
        assert!(listening_under(&proc.0, 8081).is_empty());
        // A wildcard socket is reachable over loopback and counts.
        let wildcard = Fake::new("wildcard");
        wildcard.listening("00000000", 8080, 77).holds(901, 77);
        assert_eq!(listening_under(&wildcard.0, 8080), vec![901]);
    }

    /// The whole point: a process Steam started says which game it is, and it
    /// says so in a number rather than in a name somebody chose.
    #[test]
    fn a_game_steam_started_says_which_game_it_is() {
        let proc = Fake::new("plain");
        proc.process(
            900,
            1,
            &[("SteamAppId", "504230"), ("SteamGameId", "504230")],
        );
        assert_eq!(proc.game_of(900), Some(504230));
    }

    /// And a window drawn by something the game started still answers, because
    /// the variable is inherited — which is the whole reason it is worth
    /// reading at all.
    #[test]
    fn a_child_of_the_game_is_still_the_game() {
        let proc = Fake::new("child");
        proc.process(900, 1, &[("SteamAppId", "504230")]).process(
            901,
            900,
            &[("SteamAppId", "504230")],
        );
        assert_eq!(proc.game_of(901), Some(504230));
    }

    /// A wrapper that clears the environment on its way through does not hide
    /// the game behind it: the family is walked until somebody answers.
    #[test]
    fn a_wrapper_that_kept_nothing_does_not_hide_the_game() {
        let proc = Fake::new("wrapper");
        proc.process(900, 1, &[("SteamAppId", "220200")])
            .process(901, 900, &[("PATH", "/usr/bin")])
            .process(902, 901, &[("PATH", "/usr/bin")]);
        assert_eq!(proc.game_of(902), Some(220200));
    }

    /// An ordinary program says nothing, and nothing is the answer. This is the
    /// common case and the one that must not be guessed at: a browser that
    /// happened to open while a game was loading is not the game.
    #[test]
    fn a_program_nobody_launched_through_steam_says_nothing() {
        let proc = Fake::new("ordinary");
        proc.process(900, 1, &[("PATH", "/usr/bin"), ("HOME", "/home/somebody")]);
        assert_eq!(proc.game_of(900), None);
        // Nor does a process that is not there at all — asked about a moment
        // after it exited, which is ordinary rather than exceptional.
        assert_eq!(proc.game_of(4242), None);
    }

    /// `SteamAppId` is zero for something that is not a title out of the
    /// library, and a shortcut's `SteamGameId` is not an app id and does not
    /// fit in one. Neither is an answer.
    #[test]
    fn a_shortcut_somebody_added_is_not_an_app_id() {
        let proc = Fake::new("shortcut");
        proc.process(
            900,
            1,
            &[("SteamAppId", "0"), ("SteamGameId", "17650184035926540288")],
        );
        assert_eq!(proc.game_of(900), None);
    }

    /// The walk is bounded, so a process table that loops — which a real one
    /// cannot and a fabricated one can — is not a session that stops.
    #[test]
    fn the_walk_up_the_family_ends() {
        let proc = Fake::new("bounded");
        for pid in 900..920 {
            proc.process(pid, pid + 1, &[("PATH", "/usr/bin")]);
        }
        assert_eq!(proc.game_of(900), None);
        // And a parent that is its own child ends rather than spinning.
        let loops = Fake::new("loop");
        loops.process(900, 900, &[("PATH", "/usr/bin")]);
        assert_eq!(loops.game_of(900), None);
    }

    /// An environment that is not all valid text loses that one variable and
    /// nothing else.
    #[test]
    fn one_unreadable_variable_does_not_lose_the_rest() {
        let proc = Fake::new("bytes");
        let dir = proc.0.join("900");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("status"), "PPid:\t1\n").unwrap();
        let mut environ = b"WHAT=\xff\xfe".to_vec();
        environ.push(0);
        environ.extend_from_slice(b"SteamAppId=367520");
        environ.push(0);
        std::fs::write(dir.join("environ"), environ).unwrap();
        assert_eq!(proc.game_of(900), Some(367520));
    }
}
