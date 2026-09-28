//! A directory of read-only files that are not on any disk, for as long as a
//! game disc is in the drive.
//!
//! An emulator of a CD console opens a disc image: a `.cue` sheet naming the
//! tracks, and a `.bin` of raw sectors. A disc in a drive is exactly that, read
//! a sector at a time — so this is the drive, dressed as the two files, by way
//! of the kernel's FUSE: a read of the `.bin` at some offset is a READ CD of
//! the sectors under it. Nothing is copied first, which is why a disc starts as
//! soon as it has been recognised, and nothing is left behind afterwards.
//!
//! ## Written against the kernel rather than a library
//!
//! What is needed is one directory, two files, and reading: lookup, the
//! attributes, open, read, the listing and the handful of courtesies the kernel
//! expects answered. That is a small, stable part of the kernel's protocol and
//! it is written out here, on `libc`, rather than taken from a FUSE crate that
//! would bring the rest of the protocol and its own view of mounting with it.
//!
//! Mounting is `fusermount3`'s, as it is for every unprivileged FUSE
//! filesystem: it opens `/dev/fuse`, mounts, and hands the descriptor back over
//! a socket. Asked with `auto_unmount` where it allows that, so that this
//! process ending any way at all — including being killed — takes the mount
//! with it; where it does not, a mount left behind is cleared by the next one
//! at the same place before it is made. See [`clear`].

use std::ffi::OsStr;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// One file of the directory: its name and how long it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub size: u64,
}

/// What the directory holds, and how its files are read.
pub trait Contents {
    /// The files, fixed for the life of the mount.
    fn entries(&self) -> &[Entry];

    /// Fill `into` from `offset` in file number `file`, answering how much was
    /// filled — less than asked only at the end of the file.
    fn read(&mut self, file: usize, offset: u64, into: &mut [u8]) -> io::Result<usize>;
}

/// The root directory's inode, which the kernel fixes at 1.
const ROOT: u64 = 1;

/// How long the kernel may believe what it was told. The files never change
/// while they exist, so this is only a bound on how long a stale answer about
/// a directory that has gone could live.
const VALID_SECONDS: u64 = 3600;

/// The largest request the kernel sends: a read of 128 KiB, and its header.
const REQUEST_BUFFER: usize = 128 * 1024 + 4096;

// Opcodes, from `<linux/fuse.h>`.
const LOOKUP: u32 = 1;
const FORGET: u32 = 2;
const GETATTR: u32 = 3;
const OPEN: u32 = 14;
const READ: u32 = 15;
const STATFS: u32 = 17;
const RELEASE: u32 = 18;
const FLUSH: u32 = 25;
const INIT: u32 = 26;
const OPENDIR: u32 = 27;
const READDIR: u32 = 28;
const RELEASEDIR: u32 = 29;
const ACCESS: u32 = 34;
const INTERRUPT: u32 = 36;
const DESTROY: u32 = 38;
const BATCH_FORGET: u32 = 42;

/// `FOPEN_KEEP_CACHE`: what the kernel has read of a file may be kept between
/// opens, because the file never changes.
const KEEP_CACHE: u32 = 1 << 1;

/// The protocol this speaks: 7.31, which every kernel for years has taken.
const MAJOR: u32 = 7;
const MINOR: u32 = 31;

/// A mounted directory.
pub struct Mount {
    fuse: OwnedFd,
    /// The socket `fusermount3` handed the descriptor over. Kept open because
    /// with `auto_unmount` its closing is what tells `fusermount3` to unmount.
    comm: Option<OwnedFd>,
    /// With `auto_unmount`, `fusermount3` itself, which stays to watch that
    /// socket and is reaped once it has done its work.
    watcher: Option<std::process::Child>,
}

impl Drop for Mount {
    fn drop(&mut self) {
        drop(self.comm.take());
        if let Some(mut watcher) = self.watcher.take() {
            let _ = watcher.wait();
        }
    }
}

impl Mount {
    /// Mount an empty directory `at` as this process's.
    pub fn new(at: &Path) -> io::Result<Mount> {
        clear(at);
        std::fs::create_dir_all(at)?;
        match mount_with(at, true) {
            Ok(mount) => Ok(mount),
            // `auto_unmount` is refused by a `fusermount3` that wants it paired
            // with `allow_other`, which a machine has to be configured to allow.
            // The mount without it is the same mount; what it loses is the
            // cleanup if this process is killed, which [`clear`] makes up for.
            Err(err) => {
                eprintln!("disc: mounting with auto_unmount failed ({err}); mounting without");
                mount_with(at, false)
            }
        }
    }

    /// Answer the kernel until the directory is unmounted.
    pub fn serve(&self, contents: &mut dyn Contents) -> io::Result<()> {
        let mut request = vec![0u8; REQUEST_BUFFER];
        loop {
            // SAFETY: the buffer is as long as the length given.
            let read = unsafe {
                libc::read(
                    self.fuse.as_raw_fd(),
                    request.as_mut_ptr().cast(),
                    request.len(),
                )
            };
            if read < 0 {
                let err = io::Error::last_os_error();
                match err.raw_os_error() {
                    // Unmounted: the one ordinary way out.
                    Some(libc::ENODEV) => return Ok(()),
                    // A request withdrawn before it was read, or a signal.
                    Some(libc::ENOENT | libc::EINTR | libc::EAGAIN) => continue,
                    _ => return Err(err),
                }
            }
            let request = &request[..read as usize];
            match answer(request, contents) {
                Answer::Reply(unique, reply) => self.reply(unique, reply)?,
                Answer::Nothing => {}
                Answer::Stop(unique) => {
                    self.reply(unique, Ok(Vec::new()))?;
                    return Ok(());
                }
            }
        }
    }

    fn reply(&self, unique: u64, reply: Result<Vec<u8>, i32>) -> io::Result<()> {
        let (error, payload) = match reply {
            Ok(payload) => (0, payload),
            Err(errno) => (-errno, Vec::new()),
        };
        let mut out = Vec::with_capacity(16 + payload.len());
        out.extend(((16 + payload.len()) as u32).to_ne_bytes());
        out.extend(error.to_ne_bytes());
        out.extend(unique.to_ne_bytes());
        out.extend(payload);
        // SAFETY: the buffer is as long as the length given.
        let written = unsafe { libc::write(self.fuse.as_raw_fd(), out.as_ptr().cast(), out.len()) };
        if written < 0 {
            let err = io::Error::last_os_error();
            // The request was interrupted and the kernel no longer wants it.
            if err.raw_os_error() == Some(libc::ENOENT) {
                return Ok(());
            }
            return Err(err);
        }
        Ok(())
    }
}

/// Unmount `at`, and wait for nothing: `-z` detaches it now even if something
/// still has a file in it open, which is a game that is about to be told its
/// disc has gone anyway.
pub fn unmount(at: &Path) {
    let _ = Command::new(fusermount())
        .arg("-u")
        .arg("-z")
        .arg("--")
        .arg(at)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Take down whatever an earlier run left mounted at `at`.
///
/// A mount whose server has gone answers every question about itself with
/// "transport endpoint is not connected", which is how one is told from a
/// directory. A plain directory is left alone.
pub fn clear(at: &Path) {
    if let Err(err) = std::fs::metadata(at) {
        if err.raw_os_error() == Some(libc::ENOTCONN) {
            eprintln!("disc: clearing a mount left at {}", at.display());
            unmount(at);
        }
    }
}

/// The program that mounts for somebody who is not root: FUSE 3's, then the
/// older one's.
fn fusermount() -> &'static str {
    let found = |name: &str| {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(name).is_file()))
    };
    if found("fusermount3") || !found("fusermount") {
        "fusermount3"
    } else {
        "fusermount"
    }
}

fn mount_with(at: &Path, auto_unmount: bool) -> io::Result<Mount> {
    let mut pair: [RawFd; 2] = [0; 2];
    // SAFETY: `pair` has room for the two descriptors.
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            pair.as_mut_ptr(),
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both were just opened, and each is owned exactly once.
    let (ours, theirs) = unsafe { (OwnedFd::from_raw_fd(pair[0]), OwnedFd::from_raw_fd(pair[1])) };
    // `fusermount3` finds its end by number, so that end has to survive exec.
    // SAFETY: an fcntl on a descriptor this process owns.
    if unsafe { libc::fcntl(theirs.as_raw_fd(), libc::F_SETFD, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }

    let mut options =
        "ro,nosuid,nodev,noatime,default_permissions,fsname=lxb-disc,subtype=lxb-disc".to_string();
    if auto_unmount {
        options.push_str(",auto_unmount");
    }
    let mut child = Command::new(fusermount())
        .arg("-o")
        .arg(&options)
        .arg("--")
        .arg(at)
        .env("_FUSE_COMMFD", theirs.as_raw_fd().to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()?;
    drop(theirs);

    let fuse = match receive_fd(&ours) {
        Ok(fd) => fd,
        Err(err) => {
            let status = child.wait()?;
            return Err(io::Error::other(format!(
                "fusermount3 mounted nothing ({status}): {err}"
            )));
        }
    };
    // With `auto_unmount` it stays, watching the socket, until this process
    // lets go of it; without, it is already done.
    let watcher = match auto_unmount {
        true => Some(child),
        false => {
            child.wait()?;
            None
        }
    };
    Ok(Mount {
        fuse,
        comm: Some(ours),
        watcher,
    })
}

/// The descriptor `fusermount3` sends over the socket, as `SCM_RIGHTS`.
fn receive_fd(socket: &OwnedFd) -> io::Result<OwnedFd> {
    let mut byte = [0u8; 1];
    let mut iov = libc::iovec {
        iov_base: byte.as_mut_ptr().cast(),
        iov_len: 1,
    };
    // SAFETY: CMSG_SPACE is a pure size computation.
    let space = unsafe { libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) } as usize;
    let mut control = vec![0u8; space];
    // SAFETY: an all-zero msghdr is a valid empty one.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &mut iov;
    message.msg_iovlen = 1;
    message.msg_control = control.as_mut_ptr().cast();
    message.msg_controllen = space as _;
    let received = loop {
        // SAFETY: every buffer the header points at outlives the call.
        let received =
            unsafe { libc::recvmsg(socket.as_raw_fd(), &mut message, libc::MSG_CMSG_CLOEXEC) };
        if received < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
            continue;
        }
        break received;
    };
    if received < 0 {
        return Err(io::Error::last_os_error());
    }
    if received == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "the socket closed with nothing on it",
        ));
    }
    // SAFETY: the header was filled in by recvmsg, and the macros walk only the
    // control buffer it describes.
    unsafe {
        let header = libc::CMSG_FIRSTHDR(&message);
        if header.is_null()
            || (*header).cmsg_level != libc::SOL_SOCKET
            || (*header).cmsg_type != libc::SCM_RIGHTS
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "no descriptor came with it",
            ));
        }
        let fd = std::ptr::read_unaligned(libc::CMSG_DATA(header).cast::<RawFd>());
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

/// What one request is answered with.
enum Answer {
    /// Its unique number, and the payload or an errno.
    Reply(u64, Result<Vec<u8>, i32>),
    /// Some requests are not answered at all: FORGET, INTERRUPT.
    Nothing,
    /// DESTROY: answer it and stop.
    Stop(u64),
}

/// A request's fixed header, which is 40 bytes before its own arguments.
struct Header {
    opcode: u32,
    unique: u64,
    node: u64,
}

fn header(request: &[u8]) -> Option<Header> {
    Some(Header {
        opcode: u32::from_ne_bytes(request.get(4..8)?.try_into().ok()?),
        unique: u64::from_ne_bytes(request.get(8..16)?.try_into().ok()?),
        node: u64::from_ne_bytes(request.get(16..24)?.try_into().ok()?),
    })
}

fn answer(request: &[u8], contents: &mut dyn Contents) -> Answer {
    let Some(head) = header(request) else {
        return Answer::Nothing;
    };
    let body = request.get(40..).unwrap_or_default();
    let reply = match head.opcode {
        INIT => Ok(init(body)),
        LOOKUP => lookup(body, contents.entries()),
        GETATTR => attributes(head.node, contents.entries()).map(|attr| {
            let mut out = Vec::with_capacity(104);
            out.extend(VALID_SECONDS.to_ne_bytes());
            out.extend(0u32.to_ne_bytes());
            out.extend(0u32.to_ne_bytes());
            out.extend(attr);
            out
        }),
        OPEN => open(head.node, body, contents.entries()),
        OPENDIR if head.node == ROOT => Ok(opened(0)),
        OPENDIR => Err(libc::ENOTDIR),
        READ => read(head.node, body, contents),
        READDIR => listing(head.node, body, contents.entries()),
        STATFS => Ok(statfs(contents.entries())),
        RELEASE | RELEASEDIR | FLUSH => Ok(Vec::new()),
        // With `default_permissions` the kernel decides this itself, and asks
        // only where a program asks outright. Nothing here may be written.
        ACCESS => {
            let mask = body
                .get(0..4)
                .and_then(|raw| raw.try_into().ok())
                .map_or(0, u32::from_ne_bytes);
            if mask & libc::W_OK as u32 != 0 {
                Err(libc::EROFS)
            } else {
                Ok(Vec::new())
            }
        }
        FORGET | BATCH_FORGET | INTERRUPT => return Answer::Nothing,
        DESTROY => return Answer::Stop(head.unique),
        // Everything that would change something, and the extended attributes,
        // which the kernel stops asking about once told they are not here.
        _ => Err(libc::ENOSYS),
    };
    Answer::Reply(head.unique, reply)
}

/// `fuse_init_out`: the version this speaks, and no optional behaviour.
fn init(body: &[u8]) -> Vec<u8> {
    let readahead = body
        .get(8..12)
        .and_then(|raw| raw.try_into().ok())
        .map_or(128 * 1024, u32::from_ne_bytes);
    let mut out = Vec::with_capacity(64);
    out.extend(MAJOR.to_ne_bytes());
    out.extend(MINOR.to_ne_bytes());
    out.extend(readahead.to_ne_bytes());
    out.extend(0u32.to_ne_bytes()); // flags
    out.extend(16u16.to_ne_bytes()); // max_background
    out.extend(12u16.to_ne_bytes()); // congestion_threshold
    out.extend(4096u32.to_ne_bytes()); // max_write, for writes that never come
    out.extend(1u32.to_ne_bytes()); // time_gran
    out.resize(64, 0);
    out
}

/// The inode a file is under: the root is 1, the files follow it.
fn inode_of(file: usize) -> u64 {
    file as u64 + 2
}

fn file_of(node: u64, entries: &[Entry]) -> Option<usize> {
    let file = usize::try_from(node.checked_sub(2)?).ok()?;
    (file < entries.len()).then_some(file)
}

/// `fuse_attr` for an inode.
fn attributes(node: u64, entries: &[Entry]) -> Result<Vec<u8>, i32> {
    let (mode, size, links): (u32, u64, u32) = if node == ROOT {
        (libc::S_IFDIR | 0o555, 0, 2)
    } else {
        let file = file_of(node, entries).ok_or(libc::ENOENT)?;
        (libc::S_IFREG | 0o444, entries[file].size, 1)
    };
    // SAFETY: getuid and getgid cannot fail.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let mut out = Vec::with_capacity(88);
    out.extend(node.to_ne_bytes());
    out.extend(size.to_ne_bytes());
    out.extend(size.div_ceil(512).to_ne_bytes());
    for _ in 0..3 {
        out.extend(0u64.to_ne_bytes()); // atime, mtime, ctime
    }
    for _ in 0..3 {
        out.extend(0u32.to_ne_bytes()); // their nanoseconds
    }
    out.extend(mode.to_ne_bytes());
    out.extend(links.to_ne_bytes());
    out.extend(uid.to_ne_bytes());
    out.extend(gid.to_ne_bytes());
    out.extend(0u32.to_ne_bytes()); // rdev
    out.extend(4096u32.to_ne_bytes()); // blksize
    out.extend(0u32.to_ne_bytes()); // flags
    Ok(out)
}

/// A name up to its terminating nul.
fn name_in(body: &[u8]) -> &OsStr {
    let end = body
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(body.len());
    OsStr::from_bytes(&body[..end])
}

/// `fuse_entry_out` for a name in the root.
fn lookup(body: &[u8], entries: &[Entry]) -> Result<Vec<u8>, i32> {
    let name = name_in(body);
    let file = entries
        .iter()
        .position(|entry| OsStr::new(&entry.name) == name)
        .ok_or(libc::ENOENT)?;
    let node = inode_of(file);
    let mut out = Vec::with_capacity(128);
    out.extend(node.to_ne_bytes());
    out.extend(0u64.to_ne_bytes()); // generation
    out.extend(VALID_SECONDS.to_ne_bytes()); // entry_valid
    out.extend(VALID_SECONDS.to_ne_bytes()); // attr_valid
    out.extend(0u32.to_ne_bytes());
    out.extend(0u32.to_ne_bytes());
    out.extend(attributes(node, entries)?);
    Ok(out)
}

/// `fuse_open_out`.
fn opened(flags: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    out.extend(0u64.to_ne_bytes()); // fh
    out.extend(flags.to_ne_bytes());
    out.extend(0u32.to_ne_bytes());
    out
}

fn open(node: u64, body: &[u8], entries: &[Entry]) -> Result<Vec<u8>, i32> {
    file_of(node, entries).ok_or(libc::ENOENT)?;
    let flags = body
        .get(0..4)
        .and_then(|raw| raw.try_into().ok())
        .map_or(0, u32::from_ne_bytes);
    if flags & libc::O_ACCMODE as u32 != libc::O_RDONLY as u32 {
        return Err(libc::EROFS);
    }
    Ok(opened(KEEP_CACHE))
}

/// `fuse_read_in`'s offset and size.
fn read_request(body: &[u8]) -> Option<(u64, u32)> {
    Some((
        u64::from_ne_bytes(body.get(8..16)?.try_into().ok()?),
        u32::from_ne_bytes(body.get(16..20)?.try_into().ok()?),
    ))
}

fn read(node: u64, body: &[u8], contents: &mut dyn Contents) -> Result<Vec<u8>, i32> {
    let file = file_of(node, contents.entries()).ok_or(libc::ENOENT)?;
    let (offset, size) = read_request(body).ok_or(libc::EINVAL)?;
    let length = contents.entries()[file].size;
    if offset >= length {
        return Ok(Vec::new());
    }
    let wanted = (u64::from(size)).min(length - offset) as usize;
    let mut out = vec![0u8; wanted];
    match contents.read(file, offset, &mut out) {
        Ok(filled) => {
            out.truncate(filled);
            Ok(out)
        }
        Err(err) => {
            eprintln!("disc: a read at {offset} failed: {err}");
            Err(err.raw_os_error().unwrap_or(libc::EIO))
        }
    }
}

/// The root's listing, from the offset asked: `.` and `..`, then the files.
fn listing(node: u64, body: &[u8], entries: &[Entry]) -> Result<Vec<u8>, i32> {
    if node != ROOT {
        return Err(libc::ENOTDIR);
    }
    let (offset, size) = read_request(body).ok_or(libc::EINVAL)?;
    let mut all: Vec<(u64, &str, u32)> = vec![
        (ROOT, ".", libc::DT_DIR.into()),
        (ROOT, "..", libc::DT_DIR.into()),
    ];
    all.extend(
        entries
            .iter()
            .enumerate()
            .map(|(file, entry)| (inode_of(file), entry.name.as_str(), u32::from(libc::DT_REG))),
    );
    let mut out = Vec::new();
    for (index, (node, name, kind)) in all.iter().enumerate().skip(offset as usize) {
        let name = name.as_bytes();
        let length = (24 + name.len()).next_multiple_of(8);
        if out.len() + length > size as usize {
            break;
        }
        out.extend(node.to_ne_bytes());
        out.extend((index as u64 + 1).to_ne_bytes()); // the next entry's offset
        out.extend((name.len() as u32).to_ne_bytes());
        out.extend(kind.to_ne_bytes());
        out.extend(name);
        out.resize(out.len().next_multiple_of(8), 0);
    }
    Ok(out)
}

/// `fuse_statfs_out`: how big the files are, in 2048-byte blocks, and nothing
/// free.
fn statfs(entries: &[Entry]) -> Vec<u8> {
    let blocks: u64 = entries.iter().map(|entry| entry.size.div_ceil(2048)).sum();
    let mut out = Vec::with_capacity(80);
    out.extend(blocks.to_ne_bytes());
    out.extend(0u64.to_ne_bytes()); // bfree
    out.extend(0u64.to_ne_bytes()); // bavail
    out.extend((entries.len() as u64).to_ne_bytes()); // files
    out.extend(0u64.to_ne_bytes()); // ffree
    out.extend(2048u32.to_ne_bytes()); // bsize
    out.extend(255u32.to_ne_bytes()); // namelen
    out.extend(2048u32.to_ne_bytes()); // frsize
    out.resize(80, 0);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Two files held in memory.
    pub struct Held {
        pub entries: Vec<Entry>,
        pub data: Vec<Vec<u8>>,
    }

    impl Contents for Held {
        fn entries(&self) -> &[Entry] {
            &self.entries
        }

        fn read(&mut self, file: usize, offset: u64, into: &mut [u8]) -> io::Result<usize> {
            let data = &self.data[file];
            let from = offset as usize;
            let count = into.len().min(data.len().saturating_sub(from));
            into[..count].copy_from_slice(&data[from..from + count]);
            Ok(count)
        }
    }

    fn held() -> Held {
        Held {
            entries: vec![
                Entry {
                    name: "Game.cue".into(),
                    size: 5,
                },
                Entry {
                    name: "Game.bin".into(),
                    size: 10_000,
                },
            ],
            data: vec![
                b"TRACK".to_vec(),
                (0..10_000).map(|at| (at % 251) as u8).collect(),
            ],
        }
    }

    fn request(opcode: u32, node: u64, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend(((40 + body.len()) as u32).to_ne_bytes());
        out.extend(opcode.to_ne_bytes());
        out.extend(7u64.to_ne_bytes());
        out.extend(node.to_ne_bytes());
        out.resize(40, 0);
        out.extend(body);
        out
    }

    fn replied(answer: Answer) -> Result<Vec<u8>, i32> {
        match answer {
            Answer::Reply(7, reply) => reply,
            _ => panic!("no reply"),
        }
    }

    #[test]
    fn a_name_is_looked_up_to_its_file_and_size() {
        let mut files = held();
        let reply =
            replied(answer(&request(LOOKUP, ROOT, b"Game.bin\0"), &mut files)).expect("found");
        assert_eq!(reply.len(), 128);
        assert_eq!(u64::from_ne_bytes(reply[0..8].try_into().unwrap()), 3);
        // The attributes start 40 bytes in; the size is their second field.
        assert_eq!(
            u64::from_ne_bytes(reply[48..56].try_into().unwrap()),
            10_000
        );
        let missing = answer(&request(LOOKUP, ROOT, b"Other.bin\0"), &mut files);
        assert_eq!(replied(missing), Err(libc::ENOENT));
    }

    #[test]
    fn a_read_is_served_from_the_offset_asked_and_stops_at_the_end() {
        let mut files = held();
        let mut body = vec![0u8; 40];
        body[8..16].copy_from_slice(&9_990u64.to_ne_bytes());
        body[16..20].copy_from_slice(&4096u32.to_ne_bytes());
        let reply = replied(answer(&request(READ, 3, &body), &mut files)).expect("read");
        assert_eq!(reply.len(), 10);
        assert_eq!(reply[0], (9_990 % 251) as u8);
    }

    #[test]
    fn nothing_may_be_opened_for_writing() {
        let mut files = held();
        let body = (libc::O_RDWR as u32).to_ne_bytes();
        assert_eq!(
            replied(answer(&request(OPEN, 2, &body), &mut files)),
            Err(libc::EROFS)
        );
        let body = (libc::O_RDONLY as u32).to_ne_bytes();
        assert!(replied(answer(&request(OPEN, 2, &body), &mut files)).is_ok());
    }

    /// The listing resumes at the offset the kernel gives back, which is how a
    /// directory too long for one reply is read — and how the kernel learns it
    /// has seen everything.
    #[test]
    fn the_listing_resumes_where_it_was_left() {
        let mut files = held();
        let mut body = vec![0u8; 40];
        body[16..20].copy_from_slice(&4096u32.to_ne_bytes());
        let whole = replied(answer(&request(READDIR, ROOT, &body), &mut files)).expect("listed");
        // ".", "..", and the two files, each padded to eight bytes.
        assert_eq!(whole.len(), 32 + 32 + 32 + 32);
        body[8..16].copy_from_slice(&4u64.to_ne_bytes());
        let rest = replied(answer(&request(READDIR, ROOT, &body), &mut files)).expect("listed");
        assert!(rest.is_empty());
    }

    #[test]
    fn a_forget_is_not_answered() {
        let mut files = held();
        assert!(matches!(
            answer(&request(FORGET, 2, &[0; 8]), &mut files),
            Answer::Nothing
        ));
    }

    /// The whole of it, through the kernel: mounted with `fusermount3`, read
    /// with ordinary file calls, unmounted. Skipped quietly on a machine that
    /// cannot mount FUSE — a build container, most often — because what it
    /// proves is this code against a kernel, and there is no kernel to ask.
    #[test]
    fn the_files_are_read_through_the_kernel() {
        let at = std::env::temp_dir().join(format!("lxb-fuse-test-{}", std::process::id()));
        let mount = match Mount::new(&at) {
            Ok(mount) => mount,
            Err(err) => {
                eprintln!("no FUSE here, not proven: {err}");
                let _ = std::fs::remove_dir(&at);
                return;
            }
        };
        let serving = std::thread::spawn(move || {
            let mut files = held();
            mount.serve(&mut files)
        });
        let listed: Vec<String> = std::fs::read_dir(&at)
            .map(|dir| {
                dir.flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        let cue = std::fs::read(at.join("Game.cue"));
        let bin = std::fs::read(at.join("Game.bin"));
        let written = std::fs::write(at.join("Game.cue"), b"x");
        unmount(&at);
        let served = serving.join().expect("the server");
        let _ = std::fs::remove_dir(&at);

        assert!(served.is_ok(), "{served:?}");
        let mut listed = listed;
        listed.sort();
        assert_eq!(listed, ["Game.bin", "Game.cue"]);
        assert_eq!(cue.expect("the cue"), b"TRACK");
        assert_eq!(bin.expect("the bin"), held().data[1]);
        assert!(written.is_err());
    }
}
