//! An optical drive, spoken to directly.
//!
//! Everything a game disc is read through: the table of contents, what kind of
//! disc it is, and its sectors — a DVD's as the 2048 bytes the block device
//! hands anybody, a CD's **raw**, all 2352 bytes of each, which is the one form
//! every CD console's emulator reads a disc in.
//!
//! ## Why the commands go through SG_IO on `/dev/srN`
//!
//! A CD sector is not 2048 bytes. A PlayStation disc's films and speech are in
//! sectors with 2324 bytes of data each, and its music, like a Sega CD's, is
//! plain CD audio with no data format at all — none of which the block device
//! will read, because the kernel reads a CD the way a data CD is read. The one
//! command that hands back a sector whole is SCSI's READ CD, and SG_IO is how a
//! program sends one.
//!
//! RetroArch has a disc reader of its own that does exactly this, and it opens
//! `/dev/sgN` to do it — the SCSI generic driver, which is not loaded on every
//! machine and is not something to load behind somebody's back. The block
//! device takes the same command: the kernel lets through the reading ones to
//! anybody who may read the device, and the seat's access list already says the
//! person at the machine may. So nothing about the system changes.
//!
//! The device is opened with `O_NONBLOCK`, which on Linux opens a drive without
//! locking its tray: the button on the front goes on working while a game is
//! being served off it, as it does on a console.

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;

/// The bytes in one raw CD sector: sync, header, data and error correction.
pub const RAW: usize = 2352;

/// The bytes of data in a sector as a data disc is read — every DVD sector,
/// and a CD's in Mode 1 or Mode 2 Form 1.
pub const USER: usize = 2048;

/// How many raw sectors one READ CD asks for at most.
///
/// Well under what a USB bridge will carry in one transfer — the drive this was
/// written against takes 120 KiB — and enough that one request is a useful
/// stretch of a game's data rather than a sector at a time.
pub const BATCH: u32 = 32;

/// How long one command may take before the drive is given up on. A disc that
/// has to spin up takes a couple of seconds; a scratched one is retried by the
/// drive itself for longer.
const TIMEOUT_MS: u32 = 30_000;

/// `SG_IO`, from `<scsi/sg.h>`.
const SG_IO: libc::c_ulong = 0x2285;
/// `SG_DXFER_FROM_DEV`: the drive sends, this reads.
const FROM_DEVICE: libc::c_int = -3;

/// `struct sg_io_hdr`, which the `libc` crate does not declare.
#[repr(C)]
struct SgIoHdr {
    interface_id: libc::c_int,
    dxfer_direction: libc::c_int,
    cmd_len: libc::c_uchar,
    mx_sb_len: libc::c_uchar,
    iovec_count: libc::c_ushort,
    dxfer_len: libc::c_uint,
    dxferp: *mut libc::c_void,
    cmdp: *const libc::c_uchar,
    sbp: *mut libc::c_uchar,
    timeout: libc::c_uint,
    flags: libc::c_uint,
    pack_id: libc::c_int,
    usr_ptr: *mut libc::c_void,
    status: libc::c_uchar,
    masked_status: libc::c_uchar,
    msg_status: libc::c_uchar,
    sb_len_wr: libc::c_uchar,
    host_status: libc::c_ushort,
    driver_status: libc::c_ushort,
    resid: libc::c_int,
    duration: libc::c_uint,
    info: libc::c_uint,
}

/// One track, as the table of contents gives it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Track {
    pub number: u8,
    /// Where it starts, in sectors from the start of the disc.
    pub start: u32,
    /// Whether it is data rather than music — the control field's data bit.
    pub data: bool,
}

/// A disc's table of contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toc {
    pub tracks: Vec<Track>,
    /// Where the disc ends, which is where the last track ends.
    pub leadout: u32,
    /// Whether the disc is a DVD, whose sectors are 2048 bytes and nothing else.
    pub dvd: bool,
}

impl Toc {
    /// The first track that holds data, where every console's discs keep what
    /// says which console they are for.
    pub fn first_data(&self) -> Option<&Track> {
        self.tracks.iter().find(|track| track.data)
    }

    /// Where a track ends: the next one's start, or the end of the disc.
    pub fn end_of(&self, track: &Track) -> u32 {
        self.tracks
            .iter()
            .find(|other| other.number > track.number)
            .map_or(self.leadout, |next| next.start)
    }
}

/// A source of a disc's sectors: the drive, or — in the tests, and for trying
/// the whole path without a disc — an image of one.
pub trait Sectors {
    fn toc(&self) -> &Toc;

    /// `count` raw sectors from `lba` into `into`, which is `count * RAW` long.
    /// A DVD has none and answers with an error.
    fn raw(&mut self, lba: u32, count: u32, into: &mut [u8]) -> io::Result<()>;

    /// One sector's data, as a data disc is read.
    ///
    /// Out of the raw sector for a CD, which is where it is for both kinds of
    /// data sector a game disc has; see [`user_data`].
    fn user(&mut self, lba: u32) -> io::Result<Vec<u8>> {
        let mut raw = vec![0; RAW];
        self.raw(lba, 1, &mut raw)?;
        user_data(&raw)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a data sector"))
    }
}

/// The 2048 bytes of data in a raw data sector, by the mode in its header.
///
/// Mode 1 keeps them straight after the 16-byte header; Mode 2 Form 1 — what a
/// PlayStation's files are in — after a further 8-byte subheader. A Form 2
/// sector holds 2324 bytes and is never a file system's, so it answers `None`,
/// as does anything without a sync pattern: a sector of music.
pub fn user_data(raw: &[u8]) -> Option<&[u8]> {
    const SYNC: [u8; 12] = [
        0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0,
    ];
    if raw.len() < RAW || raw[..12] != SYNC {
        return None;
    }
    match raw[15] {
        1 => Some(&raw[16..16 + USER]),
        // The submode byte's Form 2 bit, which the subheader carries twice.
        2 if raw[18] & 0x20 == 0 => Some(&raw[24..24 + USER]),
        _ => None,
    }
}

/// The drive itself.
pub struct Drive {
    file: File,
    toc: Toc,
}

impl Drive {
    /// Open the drive and read what is in it.
    pub fn open(device: &Path) -> io::Result<Drive> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(device)?;
        let mut drive = Drive {
            file,
            toc: Toc {
                tracks: Vec::new(),
                leadout: 0,
                dvd: false,
            },
        };
        drive.toc = drive.read_toc()?;
        Ok(drive)
    }

    /// Send one command whose answer comes back into `into`.
    fn command(&self, cdb: &[u8], into: &mut [u8]) -> io::Result<()> {
        let mut sense = [0u8; 32];
        let mut header = SgIoHdr {
            interface_id: libc::c_int::from(b'S'),
            dxfer_direction: FROM_DEVICE,
            cmd_len: cdb.len() as libc::c_uchar,
            mx_sb_len: sense.len() as libc::c_uchar,
            iovec_count: 0,
            dxfer_len: into.len() as libc::c_uint,
            dxferp: into.as_mut_ptr().cast(),
            cmdp: cdb.as_ptr(),
            sbp: sense.as_mut_ptr(),
            timeout: TIMEOUT_MS,
            flags: 0,
            pack_id: 0,
            usr_ptr: std::ptr::null_mut(),
            status: 0,
            masked_status: 0,
            msg_status: 0,
            sb_len_wr: 0,
            host_status: 0,
            driver_status: 0,
            resid: 0,
            duration: 0,
            info: 0,
        };
        // SAFETY: every pointer in the header points into a buffer that
        // outlives the call, and its length is the one declared beside it.
        let done = unsafe { libc::ioctl(self.file.as_raw_fd(), SG_IO, &mut header) };
        if done < 0 {
            return Err(io::Error::last_os_error());
        }
        if header.status != 0 || header.host_status != 0 || header.driver_status & 0x0f != 0 {
            let key = if header.sb_len_wr > 2 {
                sense[2] & 0x0f
            } else {
                0
            };
            return Err(io::Error::other(format!(
                "the drive refused command {:#04x}: status {:#x}, sense key {key:#x}",
                cdb[0], header.status
            )));
        }
        Ok(())
    }

    /// READ TOC, format 0, in sector numbers rather than minutes and seconds.
    fn read_toc(&self) -> io::Result<Toc> {
        let mut answer = vec![0u8; 804];
        self.command(&[0x43, 0, 0, 0, 0, 0, 0, 0x03, 0x24, 0], &mut answer)?;
        let mut toc = parse_toc(&answer).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "an unreadable table of contents",
            )
        })?;
        toc.dvd = match self.profile() {
            Some(profile) => is_dvd_profile(profile),
            // A drive too old to say what is in it: a CD holds at most eighty
            // minutes, and anything longer is not one.
            None => toc.leadout > 405_000,
        };
        Ok(toc)
    }

    /// GET CONFIGURATION's current profile: what kind of disc the drive is
    /// reading, by MMC's numbers.
    fn profile(&self) -> Option<u16> {
        let mut answer = [0u8; 8];
        self.command(&[0x46, 0x01, 0, 0, 0, 0, 0, 0, 8, 0], &mut answer)
            .ok()?;
        Some(u16::from_be_bytes([answer[6], answer[7]]))
    }

    /// One sector of a DVD, which is what the block device reads.
    fn read_dvd(&self, lba: u32) -> io::Result<Vec<u8>> {
        let mut sector = vec![0u8; USER];
        self.file
            .read_exact_at(&mut sector, u64::from(lba) * USER as u64)?;
        Ok(sector)
    }
}

impl Sectors for Drive {
    fn toc(&self) -> &Toc {
        &self.toc
    }

    /// READ CD for any kind of sector, asking for everything but the
    /// subchannels: sync, both headers, the data and its error correction. For
    /// a sector of music the drive hands back its 2352 bytes of sound, which is
    /// how an image of a disc stores it too.
    fn raw(&mut self, lba: u32, count: u32, into: &mut [u8]) -> io::Result<()> {
        if self.toc.dvd {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a DVD has no raw sectors",
            ));
        }
        let [a, b, c, d] = lba.to_be_bytes();
        let [_, e, f, g] = count.to_be_bytes();
        self.command(&[0xbe, 0, a, b, c, d, e, f, g, 0xf8, 0, 0], into)
    }

    fn user(&mut self, lba: u32) -> io::Result<Vec<u8>> {
        if self.toc.dvd {
            return self.read_dvd(lba);
        }
        let mut raw = vec![0; RAW];
        self.raw(lba, 1, &mut raw)?;
        user_data(&raw)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a data sector"))
    }
}

/// Whether an MMC profile is a DVD's — or anything later, which reads the same
/// way and is certainly not a CD.
fn is_dvd_profile(profile: u16) -> bool {
    !matches!(profile, 0x08..=0x0a)
}

/// READ TOC's answer: a four-byte header, then eight bytes a track, ending with
/// the lead-out (track 0xAA).
fn parse_toc(answer: &[u8]) -> Option<Toc> {
    let length = usize::from(u16::from_be_bytes([*answer.first()?, *answer.get(1)?]));
    let end = (length + 2).min(answer.len());
    let mut tracks = Vec::new();
    let mut leadout = None;
    for entry in answer.get(4..end)?.chunks_exact(8) {
        let number = entry[2];
        let start = u32::from_be_bytes([entry[4], entry[5], entry[6], entry[7]]);
        if number == 0xaa {
            leadout = Some(start);
        } else {
            tracks.push(Track {
                number,
                start,
                data: entry[1] & 0x04 != 0,
            });
        }
    }
    let leadout = leadout?;
    (!tracks.is_empty()).then_some(Toc {
        tracks,
        leadout,
        dvd: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The answer this machine's drive gave for a PlayStation 2 DVD: one data
    /// track, and the lead-out 2,185,680 sectors in.
    #[test]
    fn a_table_of_contents_is_read_the_way_the_drive_sent_it() {
        let mut answer = vec![0x00, 0x12, 1, 1];
        answer.extend([0, 0x14, 1, 0, 0, 0, 0, 0]);
        answer.extend([0, 0x14, 0xaa, 0, 0, 0x21, 0x59, 0xd0]);
        let toc = parse_toc(&answer).expect("a table");
        assert_eq!(
            toc.tracks,
            [Track {
                number: 1,
                start: 0,
                data: true
            }]
        );
        assert_eq!(toc.leadout, 2_185_680);
    }

    /// A CD whose first track is music, which is how a PC Engine CD begins:
    /// the data track is the second, and it ends where the third begins.
    #[test]
    fn the_first_data_track_need_not_be_the_first_track() {
        let toc = Toc {
            tracks: vec![
                Track {
                    number: 1,
                    start: 0,
                    data: false,
                },
                Track {
                    number: 2,
                    start: 3000,
                    data: true,
                },
                Track {
                    number: 3,
                    start: 9000,
                    data: false,
                },
            ],
            leadout: 12000,
            dvd: false,
        };
        let data = toc.first_data().expect("a data track");
        assert_eq!(data.number, 2);
        assert_eq!(toc.end_of(data), 9000);
        assert_eq!(toc.end_of(&toc.tracks[2]), 12000);
    }

    #[test]
    fn user_data_is_found_by_the_mode_in_the_header() {
        let mut mode1 = vec![0u8; RAW];
        mode1[1..11].fill(0xff);
        mode1[15] = 1;
        mode1[16] = 0xaa;
        assert_eq!(user_data(&mode1).map(|data| data[0]), Some(0xaa));

        let mut form1 = mode1.clone();
        form1[15] = 2;
        form1[24] = 0xbb;
        assert_eq!(user_data(&form1).map(|data| data[0]), Some(0xbb));

        let mut form2 = form1.clone();
        form2[18] = 0x20;
        assert!(user_data(&form2).is_none());

        let music = vec![0x12u8; RAW];
        assert!(user_data(&music).is_none());
    }

    #[test]
    fn a_cd_is_told_from_a_dvd_by_its_profile() {
        assert!(!is_dvd_profile(0x08));
        assert!(!is_dvd_profile(0x0a));
        assert!(is_dvd_profile(0x10));
        assert!(is_dvd_profile(0x41));
    }
}
