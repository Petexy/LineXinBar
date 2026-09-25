//! Bounded queries/metadata refreshes and interactive transactions. Only probes have a
//! timeout. A transaction is never killed because its output or UI is quiet.
use anyhow::{bail, Context, Result};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Take a `flock` on `file`, saying whether it was taken.
///
/// `Ok(false)` means somebody else holds it. Every *other* failure is an
/// error and is reported as one, which is the whole point of this existing:
/// every lock in this crate used to read `flock(…) != 0` as "somebody else
/// holds it", and a signal arriving mid-syscall makes `flock` return `EINTR`
/// whatever the lock's state is. This coordinator reaps native tools, so
/// `SIGCHLD` lands on it constantly — a tool finishing at the wrong
/// microsecond told the user "Updates are in progress. Please wait until
/// they finish." with nothing in progress, and told the coordinator another
/// coordinator already had the door, so it did not start. Found as a test
/// that failed about one run in three, and only alongside tests that spawn
/// processes.
///
/// `EINTR` is retried rather than reported: `LOCK_NB` never blocks, so there
/// is no wait here for a signal to be interrupting.
pub(crate) fn flock(file: &File, mode: i32) -> Result<bool> {
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), mode | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EWOULDBLOCK) => Ok(false),
            _ => Err(anyhow::Error::from(error).context("Cannot lock")),
        };
    }
}

/// A single native response. No reallocation, serialization or debug output;
/// errors and normal completion both erase the buffer, including partial reads.
pub(crate) struct Input {
    bytes: Box<[u8; 4096]>,
    len: usize,
}
impl Input {
    pub(crate) fn read(reader: &mut impl Read) -> Result<Self> {
        let mut input = Self {
            bytes: Box::new([0; 4096]),
            len: 0,
        };
        while input.len < input.bytes.len() {
            let at = input.len;
            reader.read_exact(&mut input.bytes[at..at + 1])?;
            if input.bytes[at] == b'\n' {
                input.bytes[at] = 0;
                return Ok(input);
            }
            input.len += 1;
        }
        bail!("The native response exceeded its limit");
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}
impl Drop for Input {
    fn drop(&mut self) {
        for byte in self.bytes.iter_mut() {
            unsafe {
                std::ptr::write_volatile(byte, 0);
            }
        }
    }
}

pub fn find(program: &str) -> Option<PathBuf> {
    if program.contains('/') {
        return None;
    }
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join(program))
        .find(|p| p.is_file() && p.metadata().is_ok_and(|m| m.mode() & 0o111 != 0))
}

fn command(program: &str, args: &[&str]) -> Result<Command> {
    let path = find(program).with_context(|| format!("{program} is not installed"))?;
    Ok(command_path(path, args))
}

fn command_path(path: PathBuf, args: &[&str]) -> Command {
    let mut c = Command::new(path);
    c.args(args)
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("GSETTINGS_BACKEND", "memory");
    c
}

pub struct Output {
    pub code: i32,
    pub text: String,
}

pub fn probe(program: &str, args: &[&str], codes: &[i32]) -> Result<Output> {
    probe_command(command(program, args)?, program, codes)
}

/// A query whose exit status is its answer: `pacman -Qqo`, `dpkg-query -S`
/// and `rpm -qf` say "nobody owns this" by exiting 1 with nothing but a
/// complaint on stderr, which [`probe`] rightly reads as a tool that failed.
/// Here every status comes back, and only a tool that could not be run, ran
/// past the bound or said too much is an error.
pub fn ask(program: &str, args: &[&str]) -> Result<Output> {
    let (code, out, _) = run_probe(command(program, args)?, program)?;
    Ok(Output {
        code,
        text: String::from_utf8_lossy(&out).into_owned(),
    })
}

/// Run `cmd` as [`probe`] does and ask nothing of its status, for a program
/// found somewhere other than `PATH`: a binary under a prefix, asked its
/// version.
pub(crate) fn ask_command(cmd: Command, program: &str) -> Result<Output> {
    let (code, out, _) = run_probe(cmd, program)?;
    Ok(Output {
        code,
        text: String::from_utf8_lossy(&out).into_owned(),
    })
}

pub(crate) fn probe_command(cmd: Command, program: &str, codes: &[i32]) -> Result<Output> {
    let (code, out, err) = run_probe(cmd, program)?;
    if !codes.contains(&code) || (code != 0 && out.is_empty() && !err.is_empty()) {
        let error = if err.is_empty() { &out } else { &err };
        bail!(
            "{program} exited {code}: {}",
            String::from_utf8_lossy(error)
                .chars()
                .take(1600)
                .collect::<String>()
        );
    }
    Ok(Output {
        code,
        text: String::from_utf8_lossy(&out).into_owned(),
    })
}

fn run_probe(mut cmd: Command, program: &str) -> Result<(i32, Vec<u8>, Vec<u8>)> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // The probe's children share a new group so a timed-out query cannot leave
    // a pipe open forever. This branch is never used for installation.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    fn drain(mut input: impl Read) -> std::io::Result<Vec<u8>> {
        let mut all = Vec::new();
        let mut buf = [0; 8192];
        loop {
            let n = input.read(&mut buf)?;
            if n == 0 {
                break;
            }
            if all.len() < 2_000_000 {
                all.extend_from_slice(&buf[..n.min(2_000_000 - all.len())]);
            }
        }
        Ok(all)
    }
    let (out_send, out_receive) = std::sync::mpsc::channel();
    let (err_send, err_receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = out_send.send(drain(out));
    });
    std::thread::spawn(move || {
        let _ = err_send.send(drain(err));
    });
    let start = Instant::now();
    let code = loop {
        if let Some(s) = child.try_wait()? {
            break s.code().unwrap_or(-1);
        }
        if start.elapsed() > Duration::from_secs(90) {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            bail!("{program} did not finish checking within 90 seconds");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let remaining = Duration::from_secs(90).saturating_sub(start.elapsed());
    let outputs = (|| -> Result<(Vec<u8>, Vec<u8>)> {
        Ok((
            out_receive
                .recv_timeout(remaining)
                .context("The query output did not close")??,
            err_receive
                .recv_timeout(Duration::from_secs(90).saturating_sub(start.elapsed()))
                .context("The query diagnostics did not close")??,
        ))
    })();
    let (out, err) = match outputs {
        Ok(output) => output,
        Err(error) => {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            return Err(error);
        }
    };
    if out.len() >= 2_000_000 {
        bail!("{program} returned too much data; the check is incomplete");
    }
    Ok((code, out, err))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub program: String,
    pub args: Vec<String>,
    pub root: bool,
    pub staged: bool,
    pub pulled_guix: bool,
    pub custom: Option<crate::custom::Reviewed>,
    /// Run this helper's own executable rather than a program found on
    /// `PATH` — see [`helper`].
    pub helper: bool,
}

impl Step {
    pub fn new(program: &str, args: &[&str], root: bool) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(|s| (*s).into()).collect(),
            root,
            staged: false,
            pulled_guix: false,
            custom: None,
            helper: false,
        }
    }
    /// A step this helper carries out itself: `lxb-updates release …`, which
    /// fetches, checks and installs what LineXinBar has released. There is no
    /// native tool that does that, and the helper is already the one program
    /// every root step has to be trusted as far as.
    pub fn of_helper(args: Vec<String>, root: bool) -> Self {
        Self {
            program: "lxb-updates".into(),
            args,
            root,
            staged: false,
            pulled_guix: false,
            custom: None,
            helper: true,
        }
    }
    pub fn pulled_guix(mut self) -> Self {
        self.pulled_guix = true;
        self
    }
    pub fn staged(mut self) -> Self {
        self.staged = true;
        self
    }
}

// A root command must come from a protected installation, never a user's PATH
// shim. pkexec still authorizes the actual executable through normal policy.
pub(crate) fn trusted(path: PathBuf) -> Result<PathBuf> {
    let canonical = path.canonicalize()?;
    for p in canonical.ancestors() {
        let m = p.metadata()?;
        if m.uid() != 0 || m.mode() & 0o022 != 0 {
            bail!("{} is not protected against replacement", p.display());
        }
    }
    Ok(canonical)
}

pub struct Transaction {
    pub child: std::process::Child,
    pub terminal: File,
    pub result: Option<std::sync::mpsc::Receiver<Vec<u8>>>,
}

fn guix_current(root: bool) -> Result<PathBuf> {
    let config = if root {
        // pkexec uses the target account's home. Do not derive root's profile
        // from the caller's HOME or from a hard-coded /root path.
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let mut buffer = vec![0u8; 65536];
        let code = unsafe {
            libc::getpwuid_r(
                0,
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if code != 0 || result.is_null() {
            bail!("Cannot determine root's Guix profile");
        }
        let entry = unsafe { entry.assume_init() };
        use std::os::unix::ffi::OsStrExt;
        let home = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
        PathBuf::from(std::ffi::OsStr::from_bytes(home.to_bytes())).join(".config")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
            .context("Cannot determine the user Guix profile")?
    };
    let path = config.join("guix/current/bin/guix");
    if !path.is_file() {
        bail!("guix pull did not produce {}", path.display());
    }
    Ok(path)
}

/// Where this very program is installed.
///
/// Its own path rather than `PATH`'s `lxb-updates`, so that the helper a step
/// runs is the one polkit authorized and not a second copy somebody put
/// earlier on the path. A package can replace it while a job runs — the
/// release step that installs a new LineXinBar does exactly that — and then
/// the kernel reports the old inode as `… (deleted)`; the path is still where
/// the installation keeps its helper, and the file there now is the newer one
/// the same installation put down.
pub fn helper() -> Result<PathBuf> {
    let path = std::env::current_exe().context("Cannot find the update helper")?;
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    Ok(match bytes.strip_suffix(b" (deleted)") {
        Some(kept) => PathBuf::from(std::ffi::OsStr::from_bytes(kept)),
        None => path,
    })
}

pub fn start(step: &Step) -> Result<Transaction> {
    if step.custom.is_some() {
        if step.root {
            bail!("Custom root providers require a job authorization");
        }
        return start_custom(step);
    }
    let args: Vec<&str> = step.args.iter().map(String::as_str).collect();
    let target = if step.helper {
        helper()?
    } else if step.pulled_guix {
        if step.program != "guix" {
            bail!("Invalid Guix operation");
        }
        guix_current(step.root)?
    } else {
        find(&step.program).context("The package manager is missing")?
    };
    let cmd = if step.root {
        let target = trusted(target)?;
        let mut c = Command::new(trusted(
            // NixOS exposes the privileged wrapper separately from the
            // unprivileged executable in the polkit store output.
            ["/run/wrappers/bin/pkexec"]
                .into_iter()
                .map(PathBuf::from)
                .find(|p| p.is_file())
                .or_else(|| find("pkexec"))
                .context("polkit's pkexec is required")?,
        )?);
        c.arg(target).args(&args);
        c
    } else {
        command_path(target, &args)
    };
    start_command(cmd)
}

/// Called only inside the authenticated, fixed-operation job worker.
pub(crate) fn start_authorized(step: &Step) -> Result<Transaction> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("An authorized update worker must run as root");
    }
    if step.custom.is_some() {
        return start_custom(step);
    }
    let target = if step.helper {
        helper()?
    } else if step.pulled_guix {
        guix_current(true)?
    } else {
        find(&step.program).context("The native update tool is unavailable")?
    };
    let args: Vec<_> = step.args.iter().map(String::as_str).collect();
    start_command(command_path(trusted(target)?, &args))
}

pub const SYSTEM_PATH: &str = "/run/current-system/sw/bin:/run/current-system/profile/bin:/run/current-system/profile/sbin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
fn start_custom(step: &Step) -> Result<Transaction> {
    let provider = step.custom.as_ref().context("Missing provider identity")?;
    provider.verify()?;
    let expected = provider.step()?;
    if expected != *step {
        bail!("Custom provider command differs from the reviewed operation");
    }
    let mut command = Command::new(trusted(PathBuf::from(&step.program))?);
    command
        .args(&step.args)
        .env_clear()
        .env("PATH", SYSTEM_PATH)
        .current_dir("/");
    start_with_result(command)
}
fn start_with_result(mut command: Command) -> Result<Transaction> {
    let mut fds = [-1; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut input = unsafe { File::from_raw_fd(fds[0]) };
    let output = unsafe { File::from_raw_fd(fds[1]) };
    command.env("LXB_UPDATE_RESULT_FD", "3");
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(output.as_raw_fd(), 3) < 0 || libc::fcntl(3, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut transaction = start_command(command)?;
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut result = vec![];
        let mut bytes = [0u8; 4096];
        // Drain excess too: a full result pipe must not block an active writer.
        while let Ok(n) = input.read(&mut bytes) {
            if n == 0 {
                break;
            }
            let room = 65537usize.saturating_sub(result.len());
            result.extend_from_slice(&bytes[..n.min(room)]);
        }
        let _ = send.send(result);
    });
    transaction.result = Some(receive);
    Ok(transaction)
}
pub fn custom_result(transaction: &mut Transaction) -> Result<Option<crate::custom::Applied>> {
    transaction
        .result
        .take()
        .map(|r| {
            let bytes = r.recv_timeout(Duration::from_secs(3)).context(
                "The provider result channel remained open; background writers are unsupported",
            )?;
            crate::custom::Applied::parse(&bytes)
        })
        .transpose()
}

fn start_command(mut cmd: Command) -> Result<Transaction> {
    cmd.env("LC_ALL", "C")
        .env("TERM", "dumb")
        .env("NO_COLOR", "1")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("GSETTINGS_BACKEND", "memory");
    let mut master = -1;
    let mut slave = -1;
    let winsize = libc::winsize {
        ws_row: 24,
        ws_col: crate::COLUMNS as u16,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &winsize,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let master = unsafe { File::from_raw_fd(master) };
    let slave = unsafe { File::from_raw_fd(slave) };
    for f in [&master, &slave] {
        unsafe {
            libc::fcntl(f.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    cmd.stdin(slave.try_clone()?)
        .stdout(slave.try_clone()?)
        .stderr(slave.try_clone()?);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    Ok(Transaction {
        child,
        terminal: master,
        result: None,
    })
}

pub fn secret(terminal: &File) -> bool {
    let mut settings = std::mem::MaybeUninit::<libc::termios>::uninit();
    if unsafe { libc::tcgetattr(terminal.as_raw_fd(), settings.as_mut_ptr()) } != 0 {
        return true;
    }
    unsafe { settings.assume_init().c_lflag & libc::ECHO == 0 }
}

/// A deliberately small streaming terminal sanitizer: strip escape sequences,
/// retain a bounded transcript, and render carriage-return progress in place.
///
/// Bounded by [`crate::TRANSCRIPT_LINES`], the whole job's allowance — the
/// lines a previous source's transaction left are in front of this one's,
/// and it is the coordinator that keeps the two within the one bound, by
/// giving this transcript the room the earlier lines leave. See
/// [`Transcript::with_room`].
pub struct Transcript {
    lines: Vec<String>,
    line: String,
    escape: u8,
    cr: bool,
    utf8: Vec<u8>,
    room: usize,
}
impl Default for Transcript {
    fn default() -> Self {
        Self::with_room(crate::TRANSCRIPT_LINES)
    }
}
impl Transcript {
    /// A transcript that keeps at most `room` lines.
    pub fn with_room(room: usize) -> Self {
        Self {
            lines: vec![],
            line: String::new(),
            escape: 0,
            cr: false,
            utf8: vec![],
            room: room.max(1),
        }
    }
    pub fn push_bytes(&mut self, bytes: &[u8]) {
        self.utf8.extend_from_slice(bytes);
        loop {
            match std::str::from_utf8(&self.utf8) {
                Ok(text) => {
                    let text = text.to_owned();
                    self.utf8.clear();
                    self.push(&text);
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    let text = String::from_utf8_lossy(&self.utf8[..valid]).into_owned();
                    self.push(&text);
                    self.utf8.drain(..valid);
                    if let Some(length) = error.error_len() {
                        self.utf8.drain(..length);
                        self.push("�");
                    } else {
                        break;
                    }
                }
            }
        }
    }
    pub fn push(&mut self, text: &str) {
        for c in text.chars() {
            match self.escape {
                1 => {
                    self.escape = if c == '[' {
                        2
                    } else if c == ']' {
                        3
                    } else {
                        0
                    };
                    continue;
                }
                2 => {
                    if ('@'..='~').contains(&c) {
                        self.escape = 0;
                    }
                    continue;
                }
                3 => {
                    if c == '\x07' {
                        self.escape = 0;
                    } else if c == '\x1b' {
                        self.escape = 4;
                    }
                    continue;
                }
                4 => {
                    self.escape = if c == '\\' { 0 } else { 3 };
                    continue;
                }
                _ => {}
            }
            if c == '\x1b' {
                self.escape = 1;
                continue;
            }
            if c == '\n' {
                self.lines.push(std::mem::take(&mut self.line));
                self.cr = false;
                if self.lines.len() > self.room {
                    self.lines.remove(0);
                }
            } else if c == '\r' {
                self.cr = true;
            } else if c == '\x08' {
                self.line.pop();
            } else if !c.is_control() || c == '\t' {
                if self.cr {
                    self.line.clear();
                    self.cr = false;
                }
                if self.line.len() < 1000 {
                    self.line.push(c);
                }
            }
        }
    }
    pub fn lines(&self) -> Vec<String> {
        let mut all = self.lines.clone();
        if !self.line.is_empty() {
            all.push(self.line.clone());
        }
        all
    }
}

pub fn write_line(file: &mut File, text: &[u8]) -> Result<()> {
    if text.len() > 512 || text.iter().any(|b| *b < 32 || *b == 127) {
        bail!("Only a single text response is accepted");
    }
    file.write_all(text)?;
    file.write_all(b"\n")?;
    file.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_and_escape_sequences_can_span_read_boundaries() {
        let mut transcript = Transcript::default();
        for byte in "[31m確認[0m?".as_bytes() {
            transcript.push_bytes(&[*byte]);
        }
        assert_eq!(transcript.lines(), ["確認?"]);
    }
    #[test]
    fn terminal_control_codes_do_not_reach_the_ui() {
        let mut t = Transcript::default();
        t.push("\x1b[31mhello\x1b[0m\r\n10%\r20%\r\nQuestion? ");
        assert_eq!(t.lines(), ["hello", "20%", "Question? "]);
        t.push("\x1b]52;c;secret\x07");
        assert!(!t.lines().join("").contains("secret"));
    }
}
