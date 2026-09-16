//! One polkit authorization for a finite update job. No password or reusable
//! token is cached. The root worker accepts only operations selected at startup
//! through its inherited, anonymous socket. Closing it revokes the job grant.
use crate::{
    discovery, firmware, policy, process, protection, Provider, Snapshot, Source, SourceId, System,
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    os::{
        fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
        unix::net::UnixStream,
    },
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const LIMIT: u64 = 256_000;

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    system: Option<System>,
    custom: Option<crate::custom::Reviewed>,
    flatpak: Vec<String>,
    snap: bool,
    firmware: Vec<String>,
    policy: policy::Policy,
}
impl Grant {
    fn reviewed(snapshot: &Snapshot) -> Result<Self> {
        let mut grant = Self {
            policy: policy::read()?,
            ..Self::default()
        };
        for source in &snapshot.sources {
            if !snapshot.selected.contains(&source.id)
                || !source.executable
                || source.error.is_some()
            {
                continue;
            }
            match &source.provider {
                Provider::System(system) => grant.system = Some(*system),
                Provider::Custom(provider)
                    if provider.manifest.run_as == crate::custom::RunAs::Root =>
                {
                    grant.custom = Some(provider.clone())
                }
                Provider::Flatpak { installations } => grant.flatpak = installations.clone(),
                Provider::Snap => grant.snap = true,
                Provider::Firmware => {
                    grant.firmware = source
                        .items
                        .iter()
                        .map(|item| {
                            item.detail
                                .split_whitespace()
                                .next()
                                .unwrap_or("")
                                .to_owned()
                        })
                        .collect()
                }
                _ => {}
            }
        }
        grant.validate()?;
        Ok(grant)
    }
    fn needed(&self) -> bool {
        self.custom.is_some()
            || self.system.is_some()
            || self.snap
            || !self.firmware.is_empty()
            || self.flatpak.iter().any(|s| s != "user")
    }
    fn validate(&self) -> Result<()> {
        if self.flatpak.len() > 64 || self.firmware.len() > 256 {
            bail!("Too many update targets");
        }
        if self.flatpak.iter().any(|s| {
            s.is_empty()
                || s.len() > 128
                || s.starts_with('-')
                || !s
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        }) {
            bail!("Invalid Flatpak installation identity");
        }
        if self
            .firmware
            .iter()
            .any(|s| s.len() != 40 || !s.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            bail!("Invalid firmware device identity");
        }
        Ok(())
    }
    fn step(&self, source: SourceId, index: usize) -> Result<process::Step> {
        self.validate()?;
        if policy::read()? != self.policy {
            bail!("Update policy changed. Check again before updating.");
        }
        match source {
            SourceId::System => {
                if let Some(provider) = &self.custom {
                    if index != 0 || provider.manifest.run_as != crate::custom::RunAs::Root {
                        bail!("Invalid custom provider operation");
                    }
                    return provider.step();
                }
                if !matches!(
                    self.policy.system,
                    None | Some(crate::custom::Owner::Native)
                ) {
                    bail!("Native system updates are disabled by the configured provider");
                }
                let system = self
                    .system
                    .context("System updates were not authorized for this job")?;
                if system != discovery::Host::read().system() {
                    bail!("The system provider changed");
                }
                let source = Source {
                    id: SourceId::System,
                    provider: Provider::System(system),
                    note: String::new(),
                    items: vec![],
                    excluded: vec![],
                    error: None,
                    checked: None,
                    fresh: false,
                    listed: false,
                    executable: true,
                    policy: Some(self.policy.clone()),
                };
                discovery::steps(&source)?
                    .get(index)
                    .cloned()
                    .context("Unknown system update step")
            }
            SourceId::Flatpak => {
                let scope = self
                    .flatpak
                    .get(index)
                    .context("Unknown Flatpak installation")?;
                if scope == "user" {
                    bail!("User Flatpaks must never run as root");
                }
                Ok(process::Step::new(
                    "flatpak",
                    &["update", "-y", &discovery::scope_flag(scope)],
                    true,
                ))
            }
            SourceId::Snap if self.snap && index == 0 => {
                Ok(process::Step::new("snap", &["refresh"], true))
            }
            SourceId::Firmware => firmware::installation(
                self.firmware
                    .get(index)
                    .context("Unknown firmware target")?,
            ),
            _ => bail!("This operation is outside the authorized update job"),
        }
    }
}

pub fn privileged(source: &Source, step: &process::Step, index: usize) -> bool {
    step.root
        || matches!(
            source.provider,
            Provider::Firmware | Provider::System(System::RpmOstree)
        )
        || matches!(&source.provider, Provider::Flatpak { installations } if installations.get(index).is_some_and(|s| s != "user"))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum Request {
    Start { source: SourceId, index: usize },
    Finish,
}
#[derive(Serialize, Deserialize)]
pub enum Event {
    Ready,
    Started(u32),
    Output(Vec<u8>),
    Echo(bool),
    CustomResult(crate::custom::Applied),
    Done(i32),
    Error(String),
    Closed,
}
fn send(stream: &mut UnixStream, value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut *stream, value)?;
    stream.write_all(b"\n")?;
    Ok(())
}
fn receive<T: serde::de::DeserializeOwned>(reader: &mut BufReader<UnixStream>) -> Result<T> {
    let mut line = String::new();
    reader.take(LIMIT).read_line(&mut line)?;
    if !line.ends_with('\n') {
        bail!("The update authorization channel closed or exceeded its limit");
    }
    Ok(serde_json::from_str(&line)?)
}

// Pass only the inhibitor descriptor, never a password or privilege token.
// Both peers retain a descriptor, so either process disappearing leaves the
// survivor's native operations protected. Ancillary descriptors are CLOEXEC.
fn send_guard(stream: &UnixStream, descriptor: RawFd) -> Result<()> {
    let mut byte = *b"G";
    let mut data = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut data;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as _) as _ };
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as _) as _;
        std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), descriptor);
        if libc::sendmsg(stream.as_raw_fd(), &message, libc::MSG_NOSIGNAL) != 1 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}
fn receive_guard(stream: &UnixStream) -> Result<File> {
    let mut byte = [0u8];
    let mut data = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    let mut control = [0usize; 8];
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut data;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = std::mem::size_of_val(&control);
    let count = unsafe { libc::recvmsg(stream.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
    if count < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut descriptors = Vec::new();
    unsafe {
        let mut header = libc::CMSG_FIRSTHDR(&message);
        while !header.is_null() {
            if (*header).cmsg_level == libc::SOL_SOCKET && (*header).cmsg_type == libc::SCM_RIGHTS {
                let len = ((*header).cmsg_len as usize).saturating_sub(libc::CMSG_LEN(0) as usize);
                for index in 0..len / std::mem::size_of::<RawFd>() {
                    let fd = std::ptr::read_unaligned(
                        libc::CMSG_DATA(header).cast::<RawFd>().add(index),
                    );
                    descriptors.push(File::from_raw_fd(fd));
                }
            }
            header = libc::CMSG_NXTHDR(&message, header);
        }
    }
    if count != 1
        || byte != *b"G"
        || message.msg_flags & libc::MSG_CTRUNC != 0
        || descriptors.len() != 1
    {
        bail!("The update worker did not transfer its power protection");
    }
    Ok(descriptors.pop().unwrap())
}

pub struct Session {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    child: Option<Child>,
    broken: bool,
    _power: Option<File>,
}
impl Session {
    pub fn needed(snapshot: &Snapshot) -> Result<bool> {
        Ok(Grant::reviewed(snapshot)?.needed())
    }
    pub fn open(snapshot: &Snapshot) -> Result<Self> {
        let grant = Grant::reviewed(snapshot)?;
        // A sibling application must not copy the private authorization channel
        // out of /proc/PID/fd or a core dump. No ambient polkit KEEP grant exists.
        if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) } != 0 {
            bail!("Cannot protect the update authorization channel");
        }
        let helper = process::trusted(std::env::current_exe()?)?;
        let pkexec = if std::path::Path::new("/run/wrappers/bin/pkexec").is_file() {
            std::path::PathBuf::from("/run/wrappers/bin/pkexec")
        } else {
            process::find("pkexec").context("polkit's pkexec is required")?
        };
        let (stream, peer) = UnixStream::pair()?;
        stream.set_read_timeout(Some(Duration::from_secs(180)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let reader = BufReader::with_capacity(1, stream.try_clone()?);
        let input: OwnedFd = peer.try_clone()?.into();
        let output: OwnedFd = peer.into();
        let child = Command::new(process::trusted(pkexec)?)
            .arg("--disable-internal-agent")
            .arg(helper)
            .arg("privileged-job")
            .stdin(Stdio::from(input))
            .stdout(Stdio::from(output))
            .stderr(Stdio::null())
            .spawn()?;
        let mut session = Self {
            stream,
            reader,
            child: Some(child),
            broken: false,
            _power: None,
        };
        send(&mut session.stream, &grant)?;
        match receive(&mut session.reader).context("Authorization was cancelled or the protected update worker could not start. No updates have started")? {
            Event::Ready => {}
            Event::Error(message) => bail!("{message}"),
            _ => bail!("The update worker did not authorize this job"),
        }
        session
            .stream
            .set_read_timeout(Some(Duration::from_secs(5)))?;
        session._power = Some(receive_guard(&session.stream)?);
        session.reader = BufReader::new(session.stream.try_clone()?);
        Ok(session)
    }
    pub fn input(&self) -> Result<File> {
        let fd: OwnedFd = self.stream.try_clone()?.into();
        Ok(File::from(fd))
    }
    pub fn run(
        &mut self,
        source: SourceId,
        index: usize,
        mut event: impl FnMut(Event),
    ) -> Result<()> {
        if self.broken {
            bail!("The job authorization channel was lost. No further privileged operations will start.");
        }
        // Reclassification and native locks can be quiet for minutes. EOF
        // detects worker loss; a quiet writer is never a reason to abandon it.
        self.stream.set_read_timeout(None)?;
        if let Err(error) = send(&mut self.stream, &Request::Start { source, index }) {
            self.broken = true;
            return Err(error);
        }
        loop {
            let response = match receive(&mut self.reader) {
                Ok(response) => response,
                Err(error) => {
                    self.broken = true;
                    return Err(error);
                }
            };
            match response {
                Event::Done(0) => return Ok(()),
                Event::Done(code) => bail!("The native update tool exited {code}. See Details."),
                Event::Error(message) => bail!("{message}"),
                value @ (Event::Started(_)
                | Event::Output(_)
                | Event::Echo(_)
                | Event::CustomResult(_)) => event(value),
                _ => {
                    self.broken = true;
                    bail!("Unexpected update worker response");
                }
            }
        }
    }
    pub fn close(mut self) -> Result<()> {
        if self.broken {
            bail!("The authorization channel was lost; the worker will finish its active operation and revoke the grant on disconnect.");
        }
        self.stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        send(&mut self.stream, &Request::Finish)?;
        if !matches!(receive(&mut self.reader)?, Event::Closed) {
            bail!("The update worker did not release authorization");
        }
        if let Some(mut child) = self.child.take() {
            child.wait()?;
        }
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

/// Invoked only by pkexec. The grant is immutable after the initial message;
/// subsequent messages contain indices, never commands, paths or environments.
pub fn serve() -> Result<()> {
    if unsafe { libc::geteuid() } != 0
        || std::env::var("PKEXEC_UID")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .is_none_or(|uid| uid == 0)
    {
        bail!("This worker must be started by polkit for an ordinary desktop account");
    }
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) } != 0 {
        bail!("Cannot protect the privileged worker");
    }
    // No caller-supplied tool paths or loader settings enter the root worker.
    std::env::set_var(
        "PATH",
        "/run/current-system/sw/bin:/run/current-system/profile/bin:/run/current-system/profile/sbin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
    );
    std::env::set_current_dir("/")?;
    // Unlike dup(), this clone is CLOEXEC: native tools and their hooks must
    // never inherit an extra copy of the private authorization channel.
    let fd = unsafe { BorrowedFd::borrow_raw(0) }.try_clone_to_owned()?;
    let mut stream = UnixStream::from(fd);
    stream
        .peer_addr()
        .context("Authorization requires an inherited Unix socket")?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::with_capacity(1, stream.try_clone()?);
    stream.set_read_timeout(Some(Duration::from_secs(180)))?;
    let grant: Grant = receive(&mut reader)?;
    stream.set_read_timeout(None)?;
    grant.validate()?;
    if grant.policy != policy::read()? {
        bail!("The reviewed administrator policy changed");
    }
    let protection = match protection::Protection::acquire() {
        Ok(guard) => guard,
        Err(error) => {
            send(&mut stream, &Event::Error(error.to_string()))?;
            return Ok(());
        }
    };
    let descriptor = protection.descriptor();
    serve_grant(
        stream,
        reader,
        grant,
        protection,
        process::start_authorized,
        Some(descriptor),
    )
}

fn serve_grant(
    mut stream: UnixStream,
    mut reader: BufReader<UnixStream>,
    grant: Grant,
    protection: impl Send,
    mut start: impl FnMut(&process::Step) -> Result<process::Transaction>,
    descriptor: Option<RawFd>,
) -> Result<()> {
    send(&mut stream, &Event::Ready)?;
    if let Some(descriptor) = descriptor {
        send_guard(&stream, descriptor)?;
    }
    let mut consumed = vec![];
    while let Ok(request) = receive::<Request>(&mut reader) {
        match request {
            Request::Finish => {
                drop(grant);
                drop(protection);
                send(&mut stream, &Event::Closed)?;
                return Ok(());
            }
            Request::Start { source, index } => {
                if consumed.contains(&(source, index)) {
                    send(
                        &mut stream,
                        &Event::Error("This update step was already attempted".into()),
                    )?;
                    continue;
                }
                let step = match grant.step(source, index) {
                    Ok(step) => step,
                    Err(error) => {
                        send(&mut stream, &Event::Error(error.to_string()))?;
                        continue;
                    }
                };
                // Only valid grant entries consume storage; arbitrary invalid
                // indices cannot grow a long-lived worker without bound.
                consumed.push((source, index));
                let transaction = match start(&step) {
                    Ok(transaction) => transaction,
                    Err(error) => {
                        send(&mut stream, &Event::Error(error.to_string()))?;
                        continue;
                    }
                };
                stream.set_read_timeout(Some(Duration::from_secs(3)))?;
                if forward(transaction, &mut stream, &mut reader).is_err() {
                    return Ok(());
                }
                stream.set_read_timeout(None)?;
            }
        }
    }
    Ok(())
}

// Keep the PTY and inhibitor alive even if the desktop/coordinator disappears.
// Drain orphan output and wait for the native tool; never kill a package writer.
fn forward(
    mut child: process::Transaction,
    stream: &mut UnixStream,
    reader: &mut BufReader<UnixStream>,
) -> Result<()> {
    let mut connected = send(stream, &Event::Started(child.child.id())).is_ok();
    let mut buf = [0u8; 4096];
    let mut echo_at = Instant::now();
    loop {
        let mut polls = [
            libc::pollfd {
                fd: child.terminal.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if connected {
                    reader.get_ref().as_raw_fd()
                } else {
                    -1
                },
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        unsafe {
            libc::poll(polls.as_mut_ptr(), 2, 100);
        }
        if polls[0].revents & libc::POLLIN != 0 {
            if let Ok(n) = child.terminal.read(&mut buf) {
                if n > 0 && connected {
                    connected = send(stream, &Event::Output(buf[..n].to_vec())).is_ok();
                }
            }
        }
        if connected && (polls[1].revents != 0 || !reader.buffer().is_empty()) {
            match process::Input::read(reader.get_mut()) {
                Ok(input) => {
                    if process::write_line(&mut child.terminal, input.bytes()).is_err() {
                        connected = false;
                    }
                }
                Err(_) => connected = false,
            }
        }
        if connected && echo_at.elapsed() >= Duration::from_millis(200) {
            connected = send(stream, &Event::Echo(process::secret(&child.terminal))).is_ok();
            echo_at = Instant::now();
        }
        if let Some(status) = child.child.try_wait()? {
            // Drain queued final output before reporting completion.
            let until = Instant::now() + Duration::from_millis(500);
            loop {
                if Instant::now() >= until {
                    break;
                }
                let mut ready = libc::pollfd {
                    fd: child.terminal.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                if unsafe { libc::poll(&mut ready, 1, 0) } <= 0 || ready.revents & libc::POLLIN == 0
                {
                    break;
                }
                match child.terminal.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        if connected {
                            connected = send(stream, &Event::Output(buf[..n].to_vec())).is_ok();
                        }
                    }
                    _ => break,
                }
            }
            if !connected {
                bail!("Update client disconnected");
            }
            if status.success() {
                match process::custom_result(&mut child) {
                    Ok(Some(result)) => send(stream, &Event::CustomResult(result))?,
                    Ok(None) => {}
                    Err(error) => {
                        send(stream, &Event::Error(error.to_string()))?;
                        return Ok(());
                    }
                }
            }
            send(stream, &Event::Done(status.code().unwrap_or(-1)))?;
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grants_cannot_name_commands_or_user_flatpak_root_operations() {
        assert!(serde_json::from_str::<Grant>(r#"{"command":"sh"}"#).is_err());
        let mut grant = Grant::default();
        grant.flatpak.push("--system --command=sh".into());
        assert!(grant.validate().is_err());
        grant.flatpak = vec!["user".into()];
        assert!(grant.step(SourceId::Flatpak, 0).is_err());
        assert!(grant.step(SourceId::Snap, 0).is_err());
        assert!(grant.step(SourceId::Aur, 0).is_err());
        grant.firmware.push("--force".into());
        assert!(grant.validate().is_err());
    }

    fn local_session(stream: UnixStream) -> Session {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Session {
            reader: BufReader::new(stream.try_clone().unwrap()),
            stream,
            child: None,
            broken: false,
            _power: None,
        }
    }

    struct DropNotice(std::sync::mpsc::Sender<()>);
    impl Drop for DropNotice {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }

    #[test]
    fn one_grant_runs_distinct_steps_rejects_replay_and_releases_before_close() {
        let (client, server) = UnixStream::pair().unwrap();
        let (released, release) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let reader = BufReader::with_capacity(1, server.try_clone().unwrap());
            let grant = Grant {
                snap: true,
                flatpak: vec!["system".into()],
                policy: policy::read().unwrap(),
                ..Grant::default()
            };
            let guard = File::open("/dev/null").unwrap();
            let descriptor = guard.as_raw_fd();
            let mut starts = 0;
            serve_grant(
                server,
                reader,
                grant,
                (DropNotice(released), guard),
                |_| {
                    starts += 1;
                    process::start(&process::Step::new(
                        "sh",
                        &["-c", "printf 'fixture complete\\n'"],
                        false,
                    ))
                },
                Some(descriptor),
            )
            .unwrap();
            starts
        });
        let mut session = local_session(client);
        session.reader = BufReader::with_capacity(1, session.stream.try_clone().unwrap());
        assert!(matches!(
            receive(&mut session.reader).unwrap(),
            Event::Ready
        ));
        session._power = Some(receive_guard(&session.stream).unwrap());
        session.reader = BufReader::new(session.stream.try_clone().unwrap());
        let mut output = Vec::new();
        session
            .run(SourceId::Snap, 0, |event| {
                if let Event::Output(bytes) = event {
                    output.extend(bytes);
                }
            })
            .unwrap();
        assert!(String::from_utf8_lossy(&output).contains("fixture complete"));
        assert!(session
            .run(SourceId::Snap, 0, |_| {})
            .unwrap_err()
            .to_string()
            .contains("already attempted"));
        assert!(session.run(SourceId::Aur, 0, |_| {}).is_err());
        session.run(SourceId::Flatpak, 0, |_| {}).unwrap();
        assert!(release.try_recv().is_err());
        session.close().unwrap();
        release
            .try_recv()
            .expect("authorization and protection must be gone before Closed");
        assert_eq!(worker.join().unwrap(), 2);
    }

    #[test]
    fn disconnect_drains_native_writer_before_releasing_protection() {
        let (client, server) = UnixStream::pair().unwrap();
        let (released, release) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let reader = BufReader::with_capacity(1, server.try_clone().unwrap());
            serve_grant(
                server,
                reader,
                Grant {
                    snap: true,
                    policy: policy::read().unwrap(),
                    ..Grant::default()
                },
                DropNotice(released),
                |_| {
                    process::start(&process::Step::new(
                        "sh",
                        &["-c", "sleep 0.4; printf finished"],
                        false,
                    ))
                },
                None,
            )
        });
        let mut session = local_session(client);
        assert!(matches!(
            receive(&mut session.reader).unwrap(),
            Event::Ready
        ));
        send(
            &mut session.stream,
            &Request::Start {
                source: SourceId::Snap,
                index: 0,
            },
        )
        .unwrap();
        assert!(matches!(
            receive(&mut session.reader).unwrap(),
            Event::Started(_)
        ));
        drop(session);
        assert!(
            release.recv_timeout(Duration::from_millis(100)).is_err(),
            "the live writer lost protection"
        );
        release.recv_timeout(Duration::from_secs(5)).unwrap();
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn private_response_crosses_the_worker_without_entering_output() {
        let (client, mut server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            server
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::with_capacity(1, server.try_clone().unwrap());
            let transaction = process::start(&process::Step::new("sh", &[
                "-c", "stty -echo; printf 'Private response: '; read -r answer; stty echo; test \"$answer\" = fixture-secret; printf '\\nfinished\\n'"
            ], false)).unwrap();
            forward(transaction, &mut server, &mut reader).unwrap();
        });
        let mut session = local_session(client);
        let mut output = Vec::new();
        let mut answered = false;
        loop {
            match receive(&mut session.reader).unwrap() {
                Event::Echo(true) if !answered => {
                    session.stream.write_all(b"fixture-secret\n").unwrap();
                    answered = true;
                }
                Event::Output(bytes) => output.extend(bytes),
                Event::Done(0) => break,
                Event::Started(_) | Event::Echo(_) => {}
                _ => panic!("unexpected worker event"),
            }
        }
        assert!(answered);
        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("finished"));
        assert!(!output.contains("fixture-secret"));
        worker.join().unwrap();
    }

    #[test]
    fn transferred_guard_survives_sender_and_is_not_inherited() {
        let (client, server) = UnixStream::pair().unwrap();
        let (guard, peer) = UnixStream::pair().unwrap();
        send_guard(&server, guard.as_raw_fd()).unwrap();
        let received = receive_guard(&client).unwrap();
        assert_ne!(
            unsafe { libc::fcntl(received.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        drop(guard);
        drop(server);
        let mut ready = libc::pollfd {
            fd: peer.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(
            unsafe { libc::poll(&mut ready, 1, 0) },
            0,
            "receiver must still hold the guard"
        );
        drop(received);
        assert_eq!(
            unsafe { libc::poll(&mut ready, 1, 100) },
            1,
            "last guard must close"
        );
    }

    #[test]
    fn missing_guard_fails_closed() {
        let (client, mut server) = UnixStream::pair().unwrap();
        server.write_all(b"G").unwrap();
        assert!(receive_guard(&client).is_err());
    }

    #[test]
    fn broken_channel_cannot_authorize_another_step() {
        let (client, server) = UnixStream::pair().unwrap();
        let mut session = local_session(client);
        drop(server);
        assert!(session.run(SourceId::Snap, 0, |_| {}).is_err());
        assert!(session.broken);
        assert!(session
            .run(SourceId::Flatpak, 0, |_| {})
            .unwrap_err()
            .to_string()
            .contains("No further privileged"));
        assert!(session.close().is_err());
    }
}
