//! What this machine is: the facts behind Settings > System > System
//! information.
//!
//! Everything here is something the machine says about *itself* — the name and
//! version of the system on the disk, the shell's own version, the address it
//! answers on, and what it is built out of. None of it is a setting. There is
//! nothing on this page to change, which is the whole reason it is a panel to
//! read rather than another column of the bar: a column is a list of answers,
//! and these are not answers to anything.
//!
//! Read on the press, and not kept up to date afterwards. Every reading is one
//! the kernel gives out of its own memory — `/proc`, `/etc/os-release`, one
//! `statvfs` and one `getifaddrs` — which together is a fraction of a
//! millisecond, so this needs none of the worker [`crate::appinfo`] has behind
//! it. That one starts a package manager and takes long enough to drop frames;
//! this one starts nothing at all.
//!
//! Nothing is invented. A value the machine will not give is [`unknown`] rather
//! than something guessed from a value beside it, and the one row that is not
//! always there is the one the machine may genuinely not have — a rolling
//! release has no version to give, and a page showing it an empty one would be
//! saying it had a version and had forgotten it.

use std::path::Path;

/// What a machine that will not answer gets, rather than a guess.
///
/// The same word [`crate::appinfo`] uses for a version no package manager
/// could give, and for the same reason: a panel of facts has to be able to say
/// that it does not know one without the row disappearing, because a row that
/// vanished would take the question with it.
fn unknown() -> String {
    crate::i18n::text("shell-unknown").to_string()
}

/// Everything the panel says, in the order it says it.
///
/// Strings rather than numbers, because every one of these is already the
/// answer as a person reads it: a size in bytes, a core count and an address
/// are three different shapes, and a panel built out of them would be a second
/// place where a byte count becomes `GiB`. See [`Facts::read`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// What the system on this disk calls itself — `NAME` from `os-release`.
    pub name: String,
    /// Its version, where it has one. `None` for a rolling release, which
    /// genuinely has no version to give; the row is left off rather than
    /// filled in with a word standing where a number would be.
    pub version: Option<String>,
    /// This shell's own version, as the packaging spells it.
    pub software: String,
    /// The address this machine answers on, or that it is on no network.
    pub address: String,
    /// The kernel it is running.
    pub kernel: String,
    /// The processor it is running on.
    pub processor: String,
    /// The graphics adapter the shell itself is drawing through.
    pub graphics: String,
    /// How much memory is free, of how much there is.
    pub memory: String,
    /// The same for the filesystem the system is installed on.
    pub disk: String,
}

impl Facts {
    /// Read every one of them, now.
    ///
    /// `graphics` is handed in rather than looked up, because it is the one
    /// fact on this page that no file on the disk knows: what the shell is
    /// drawing through is whichever adapter its own renderer opened, and the
    /// renderer is the only thing that can say which. `None` where there is no
    /// renderer yet, which is the shell drawing nothing at all.
    pub fn read(graphics: Option<String>) -> Self {
        let release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
        Self {
            // `NAME` rather than `PRETTY_NAME`, which on most systems is the
            // name with the version already in it — and the version is the row
            // underneath. `PRETTY_NAME` is the fallback for the same reason it
            // is not the first choice: a system that gives only one of the two
            // is better read with the version doubled than not named at all.
            name: os_release(&release, "NAME")
                .or_else(|| os_release(&release, "PRETTY_NAME"))
                .unwrap_or_else(unknown),
            version: os_release(&release, "VERSION_ID"),
            // The one number here that is not read from anywhere: it is
            // compiled in, and `build.rs` refuses the build when it has drifted
            // from the `VERSION` file at the root of the checkout. Written the
            // way a console writes it, because that is what this row is.
            software: crate::message!("software-version", "version" => env!("CARGO_PKG_VERSION")),
            address: address()
                .unwrap_or_else(|| crate::i18n::text("shell-not-connected").to_string()),
            kernel: kernel().unwrap_or_else(unknown),
            processor: read_and("/proc/cpuinfo", processor),
            graphics: graphics
                .as_deref()
                .map(adapter)
                .filter(|name| !name.is_empty())
                .unwrap_or_else(unknown),
            memory: read_and("/proc/meminfo", memory),
            // The filesystem the system is on, not the one the user's files are
            // on: this page is about the machine, and where a separate `/home`
            // has got to is a question the file explorer answers, standing in
            // the folder it is asked about.
            disk: crate::files::room(Path::new("/")).unwrap_or_else(unknown),
        }
    }
}

/// Read a file the kernel writes and hand it to `parse`, or answer [`unknown`].
///
/// One helper for the two `/proc` files, so that a machine without one of them
/// — a container with a trimmed `/proc`, or a kernel this shell has not met —
/// comes back with a row saying so rather than with an empty string that reads
/// as a value.
fn read_and(path: &str, parse: impl Fn(&str) -> Option<String>) -> String {
    std::fs::read_to_string(path)
        .ok()
        .as_deref()
        .and_then(parse)
        .unwrap_or_else(unknown)
}

/// One value out of `os-release`, unquoted.
///
/// The file is shell syntax, and every distribution quotes a different half of
/// it — `NAME="Some System"` beside `ID=some-system` in the same file. So the
/// quotes come off here rather than being left for the panel to draw, and the
/// two escapes that can appear inside a double-quoted value are undone with
/// them.
///
/// A value that is empty once unquoted is `None`, not an empty string: a
/// distribution that writes `VERSION_ID=""` is saying it has no version, and
/// the row for it must not appear blank.
fn os_release(text: &str, key: &str) -> Option<String> {
    let raw = text
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix(key)?.strip_prefix('='))
        .next()?
        .trim();
    let value = match raw
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        Some(quoted) => quoted.replace("\\\"", "\"").replace("\\\\", "\\"),
        None => raw.trim_matches('\'').to_string(),
    };
    Some(value).filter(|value| !value.is_empty())
}

/// The kernel this machine is running, named the way `uname` names it.
///
/// Read out of `/proc` rather than asked of `uname(2)`, because the two answer
/// with the same string and one of them needs no `unsafe` to get at. The system
/// name comes with it — `Linux 7.0.0-generic` rather than a bare number — so
/// that the row says what kind of kernel it is as well as which one.
fn kernel() -> Option<String> {
    let kind = read_line("/proc/sys/kernel/ostype")?;
    let release = read_line("/proc/sys/kernel/osrelease")?;
    Some(format!("{kind} {release}"))
}

/// The whole of a one-line file in `/proc`, without its newline.
fn read_line(path: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(text.trim().to_string()).filter(|line| !line.is_empty())
}

/// What the processor calls itself, with the trademark noise and the row's own
/// label taken out of it.
///
/// Verbatim otherwise. The vendor's own string carries what a person wants off
/// this row — `AMD Ryzen 5 7600 6-Core Processor` says its core count,
/// `Intel Core i7-8700K CPU @ 3.70GHz` says its clock — and rewriting it into a
/// house style would mean deciding, for every vendor there is, which half of
/// the name is the model. So nothing is rewritten. What comes off is
/// [`trademarks`], and then the two words that say "processor" a second time
/// beside a label that already does:
///
/// - a trailing `Processor`, which is where AMD puts it. Only trailing: a
///   virtualised `Intel Xeon Processor (Skylake, IBRS)` says it in the middle of
///   its own name and keeps it.
/// - a `CPU` immediately before an `@`, which is where Intel puts it. Only
///   there: `QEMU Virtual CPU version 2.5+` needs the word to say what it is.
///
/// Both were costing the end of the row rather than reading as anything. On a
/// 1080p display the answer has room for about thirty characters, and AMD's
/// string is thirty-three of them — the panel showed
/// `AMD Ryzen 5 7600 6-Core Process…`, which is the core count cut in half by a
/// word the label had already said.
///
/// The keys are tried in order and matched exactly as the kernel spells them,
/// which is the whole difference between a name and a number here. An x86
/// `/proc/cpuinfo` carries a lowercase `model` — the model *id*, `97` — several
/// lines above `model name`, so a search that took whichever came first, or
/// that ignored case, answered this row with a two-digit number. Found on
/// screen, which is the only place it looks like anything.
///
/// The first matching line, and not a count of them: every core writes its own
/// copy of the same block, so the first is the answer and the rest are that
/// answer again.
fn processor(cpuinfo: &str) -> Option<String> {
    let named = |wanted: &str| {
        cpuinfo
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(key, _)| key.trim() == wanted)
            .map(|(_, value)| value)
    };
    // `model name` is x86's. `Model` and `Hardware` are what a board gives
    // instead, where `/proc/cpuinfo` names its implementer in hexadecimal and
    // says what the machine is on one of those two lines.
    let raw = named("model name")
        .or_else(|| named("Model"))
        .or_else(|| named("Hardware"))?;
    let mut words: Vec<String> = trademarks(raw)
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if words.last().is_some_and(|last| last == "Processor") {
        words.pop();
    }
    if let Some(at) = words.iter().position(|word| word == "@") {
        if at > 0 && words[at - 1] == "CPU" {
            words.remove(at - 1);
        }
    }
    Some(words.join(" ")).filter(|name| !name.is_empty())
}

/// A hardware name with `(R)` and `(TM)` taken out and the gaps they leave
/// closed up.
///
/// The two marks are not part of any name — no other program on the machine
/// shows them to the user either — and they are the difference between
/// `Intel Core i7-8700K` and a row that has run out of room before it reaches
/// the model. Shared by the processor and the graphics adapter, which are the
/// two rows here whose value is written by a vendor.
fn trademarks(raw: &str) -> String {
    let stripped = raw
        .replace("(R)", "")
        .replace("(r)", "")
        .replace("(TM)", "")
        .replace("(tm)", "");
    let mut cleaned = String::with_capacity(stripped.len());
    for word in stripped.split_whitespace() {
        if !cleaned.is_empty() {
            cleaned.push(' ');
        }
        cleaned.push_str(word);
    }
    cleaned
}

/// The graphics adapter as the row shows it: what the card is, without the
/// driver that is carrying it.
///
/// Mesa names an adapter `AMD Radeon RX 9060 XT (RADV GFX1200)`, and radeonsi
/// goes further — `AMD Radeon RX 6800 (radeonsi, navi21, LLVM 17.0.6, DRM
/// 3.54, 6.6.0)`. Everything in that trailing bracket is the *driver*: which
/// Mesa component opened the card, the chip's internal codename, and the
/// versions of two libraries. It is a real answer to a different question, it
/// is longer than the name it follows, and on this panel it pushed the model
/// itself off the end of the row — seen in a capture, where the row read
/// `AMD Radeon RX 9060 XT (RADV G…`.
///
/// So one trailing bracketed group comes off, and only a trailing one: a name
/// like `Intel Arc A770 Graphics (DG2)` loses the codename and keeps the card,
/// while `Intel(R) UHD Graphics 620` keeps everything that is not a trademark.
/// The full string is still in the shell's log, where [`crate::gpu::Gpu`] wrote
/// it when it opened the adapter.
fn adapter(name: &str) -> String {
    let name = trademarks(name);
    match name.rsplit_once('(') {
        Some((card, driver)) if driver.ends_with(')') && !card.trim().is_empty() => {
            card.trim().to_string()
        }
        _ => name,
    }
}

/// How much memory is free, of how much this machine has.
///
/// `MemAvailable` rather than `MemFree`, which is the number every system
/// monitor was wrong about for years: a kernel with a gigabyte free and twenty
/// gigabytes of cache it would give up the moment anything asked has twenty-one
/// available, and a row saying one would be telling a user their machine was
/// full when it was empty. `MemFree` is the fallback only for a kernel too old
/// to publish the better number.
fn memory(meminfo: &str) -> Option<String> {
    let field = |name: &str| {
        meminfo
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(key, _)| key.trim() == name)
            .and_then(|(_, value)| value.split_whitespace().next()?.parse::<u64>().ok())
            // Every count in this file is in kibibytes, whatever the `kB` after
            // it says.
            .map(|kib| kib * 1024)
    };
    let total = field("MemTotal")?;
    let free = field("MemAvailable").or_else(|| field("MemFree"))?;
    if total == 0 {
        return None;
    }
    Some(
        crate::message!("disk-free-of", "free" => crate::appinfo::human_size(free), "whole" => crate::appinfo::human_size(total)),
    )
}

/// The address this machine answers on.
///
/// Asked of the kernel rather than of a network manager, for the reason the
/// mixer asks the sound server directly: a shell that is the whole session has
/// nobody to ask, and there is no guarantee any particular daemon is running.
///
/// One address, because the row is one line and a machine with a wired
/// connection, a wireless one and three virtual bridges has half a dozen. The
/// one shown is the first ordinary IPv4 address on an interface that is up and
/// is not the loopback — which is the one a person means when they ask a
/// console for its address, because it is the one to type into another machine
/// on the same network. An IPv6 address stands in only where there is no IPv4
/// at all, and neither a link-local address nor the loopback ever does: those
/// two say the machine is on no network, which is what `None` already says.
fn address() -> Option<String> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: `getifaddrs` writes one owned list into `list` and returns zero,
    // or writes nothing and returns non-zero. The list is walked and then freed
    // below, and nothing taken from it outlives the free.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return None;
    }
    let mut four: Option<std::net::Ipv4Addr> = None;
    let mut six: Option<std::net::Ipv6Addr> = None;
    let mut node = list;
    while !node.is_null() {
        // SAFETY: `node` is one link of the list `getifaddrs` wrote, checked
        // non-null on the way in and reassigned from the entry's own `ifa_next`.
        let entry = unsafe { &*node };
        node = entry.ifa_next;
        if entry.ifa_addr.is_null() {
            continue;
        }
        let flags = entry.ifa_flags as libc::c_int;
        if flags & libc::IFF_UP == 0 || flags & libc::IFF_LOOPBACK != 0 {
            continue;
        }
        // SAFETY: `ifa_addr` is non-null and points at a `sockaddr` whose
        // family says which larger structure it really is. Read unaligned:
        // nothing promises the kernel's storage is aligned for `sockaddr_in6`,
        // and the family is the one field every one of them starts with.
        let family = unsafe { std::ptr::read_unaligned(entry.ifa_addr) }.sa_family;
        match family as libc::c_int {
            libc::AF_INET if four.is_none() => {
                // SAFETY: the family says this is a `sockaddr_in`.
                let sock =
                    unsafe { std::ptr::read_unaligned(entry.ifa_addr as *const libc::sockaddr_in) };
                let address = std::net::Ipv4Addr::from(u32::from_be(sock.sin_addr.s_addr));
                if !address.is_loopback() && !address.is_link_local() && !address.is_unspecified() {
                    four = Some(address);
                }
            }
            libc::AF_INET6 if six.is_none() => {
                // SAFETY: the family says this is a `sockaddr_in6`.
                let sock = unsafe {
                    std::ptr::read_unaligned(entry.ifa_addr as *const libc::sockaddr_in6)
                };
                let address = std::net::Ipv6Addr::from(sock.sin6_addr.s6_addr);
                // `fe80::/10` is the address every interface gives itself
                // whether or not anything is plugged into it, so it is no
                // answer to "what is this machine's address".
                let link_local =
                    sock.sin6_addr.s6_addr[0] == 0xfe && sock.sin6_addr.s6_addr[1] & 0xc0 == 0x80;
                if !address.is_loopback() && !address.is_unspecified() && !link_local {
                    six = Some(address);
                }
            }
            _ => {}
        }
    }
    // SAFETY: `list` is the list `getifaddrs` wrote and nothing above kept a
    // pointer into it — both addresses were copied out by value.
    unsafe { libc::freeifaddrs(list) };
    four.map(|address| address.to_string())
        .or_else(|| six.map(|address| address.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fixture here is written by hand in the file's own shape, with
    /// invented names and round numbers. Nothing this machine reports belongs
    /// in this file: a system name or a processor pasted out of `/proc` is a
    /// test that says the feature was built for one desk, and a memory total
    /// taken from the host is a test that passes or fails by which machine ran
    /// it.
    const RELEASE: &str = r#"
NAME="Test System"
PRETTY_NAME="Test System 9"
ID=test-system
VERSION_ID="9"
HOME_URL="https://example.invalid/"
"#;

    #[test]
    fn os_release_values_lose_their_quotes() {
        assert_eq!(
            os_release(RELEASE, "NAME").as_deref(),
            Some("Test System"),
            "a quoted value is read without its quotes"
        );
        assert_eq!(os_release(RELEASE, "VERSION_ID").as_deref(), Some("9"));
        assert_eq!(
            os_release(RELEASE, "ID").as_deref(),
            Some("test-system"),
            "and an unquoted one is read as it stands"
        );
        assert_eq!(
            os_release(RELEASE, "BUILD_ID"),
            None,
            "a key the file does not carry is absent, not empty"
        );
    }

    /// A key is matched from the start of the line and in full. `VERSION` and
    /// `VERSION_ID` are two different keys, and a reader that took the first
    /// line beginning with `VERSION` would answer one with the other.
    #[test]
    fn a_key_is_not_the_start_of_a_longer_one() {
        const BOTH: &str = "VERSION_ID=9\nVERSION=\"9 (Test)\"\n";
        assert_eq!(os_release(BOTH, "VERSION").as_deref(), Some("9 (Test)"));
        assert_eq!(os_release(BOTH, "VERSION_ID").as_deref(), Some("9"));
    }

    /// A rolling release has no version, and the panel leaves the row off
    /// rather than showing an empty one. Both spellings of "no version" —
    /// the key absent, and the key present and empty — mean the same thing.
    #[test]
    fn a_system_with_no_version_says_so_by_saying_nothing() {
        const ROLLING: &str = "NAME=\"Test System\"\nBUILD_ID=rolling\n";
        const EMPTY: &str = "NAME=\"Test System\"\nVERSION_ID=\"\"\n";
        assert_eq!(os_release(ROLLING, "VERSION_ID"), None);
        assert_eq!(os_release(EMPTY, "VERSION_ID"), None);
    }

    /// Written in the shape of a `/proc/cpuinfo` block, with `model name`
    /// carrying whatever a case wants to try.
    fn cpuinfo(model_name: &str) -> String {
        format!(
            "processor\t: 0\nvendor_id\t: TestVendor\nmodel name\t: {model_name}\nsiblings\t: 4\nprocessor\t: 1\nmodel name\t: {model_name}\n"
        )
    }

    #[test]
    fn the_processor_keeps_its_name_and_loses_its_trademarks() {
        assert_eq!(
            processor(&cpuinfo("Test(R) Core(TM) X9-9000 CPU @ 3.00GHz")).as_deref(),
            Some("Test Core X9-9000 @ 3.00GHz"),
            "the marks come out and the spaces they left close up"
        );
    }

    /// The row is labelled `Processor`, so a value that ends in the word says it
    /// twice — and it said it at the cost of the end of the line, which is where
    /// the core count is. Both spellings go, and neither takes a word that is
    /// carrying meaning with it.
    #[test]
    fn the_processor_does_not_say_processor_a_second_time() {
        assert_eq!(
            processor(&cpuinfo("Test Ryzen 5 9000 6-Core Processor")).as_deref(),
            Some("Test Ryzen 5 9000 6-Core"),
            "the core count is what the trailing word was pushing off the row"
        );
        assert_eq!(
            processor(&cpuinfo("Test Xeon Processor (Testlake, IBRS)")).as_deref(),
            Some("Test Xeon Processor (Testlake, IBRS)"),
            "a machine that says it in the middle of its name keeps it"
        );
        assert_eq!(
            processor(&cpuinfo("Test Virtual CPU version 9.9")).as_deref(),
            Some("Test Virtual CPU version 9.9"),
            "and a CPU that is not standing in front of a clock keeps that"
        );
    }

    /// The bug this row was found with on screen: x86 writes a lowercase
    /// `model` — the model *id*, a small number — several lines above the name,
    /// and the name is the one this row wants.
    #[test]
    fn the_processor_is_named_rather_than_numbered() {
        const CPUINFO: &str = "\
processor\t: 0
cpu family\t: 9
model\t\t: 97
model name\t: Test X9-9000
stepping\t: 2
";
        assert_eq!(processor(CPUINFO).as_deref(), Some("Test X9-9000"));
    }

    /// A board that names itself on a `Model` line instead — which is what a
    /// machine with no x86 `model name` gives — is still answered.
    #[test]
    fn a_board_without_a_model_name_line_is_still_named() {
        const BOARD: &str = "\
processor\t: 0
CPU implementer\t: 0x00
Model\t\t: Test Board 4
";
        assert_eq!(processor(BOARD).as_deref(), Some("Test Board 4"));
        assert_eq!(
            processor("processor\t: 0\n"),
            None,
            "and one that names itself nowhere is unknown, not blank"
        );
    }

    /// The adapter row keeps the card and drops the driver behind it. The
    /// second of these is what the panel actually showed before it did — the
    /// model ran off the end of the row and the bracket was all that was left.
    #[test]
    fn the_adapter_keeps_the_card_and_drops_the_driver() {
        assert_eq!(
            adapter("Test Radeon X9000 (TESTDRV GFX9000)"),
            "Test Radeon X9000"
        );
        assert_eq!(
            adapter("Test Radeon X9000 (testdrv, chip9, LLVM 1.2.3, DRM 3.54, 9.0.0)"),
            "Test Radeon X9000",
            "however long the driver's own list of versions is"
        );
        assert_eq!(
            adapter("Test(R) Graphics 620"),
            "Test Graphics 620",
            "a trademark is not a driver, and what is left is the whole name"
        );
        assert_eq!(
            adapter("Test GeForce XT 9000"),
            "Test GeForce XT 9000",
            "a driver that puts nothing in brackets loses nothing"
        );
        assert_eq!(
            adapter("(TESTDRV GFX9000)"),
            "(TESTDRV GFX9000)",
            "and a name that is nothing but a bracket is not cut down to nothing"
        );
    }

    /// One mebibyte in kibibytes, so the arithmetic below is readable.
    const MIB: u64 = 1024;

    #[test]
    fn memory_is_what_is_available_rather_than_what_is_untouched() {
        let meminfo = format!(
            "MemTotal:       {:>8} kB\nMemFree:        {:>8} kB\nMemAvailable:   {:>8} kB\n",
            8 * 1024 * MIB,
            512 * MIB,
            6 * 1024 * MIB,
        );
        assert_eq!(
            memory(&meminfo).as_deref(),
            Some("6.0 GiB free of 8.0 GiB"),
            "the cache a kernel would give back counts as free"
        );
    }

    /// A kernel too old to publish `MemAvailable` still gets a row.
    #[test]
    fn memory_falls_back_to_what_is_untouched() {
        let meminfo = format!(
            "MemTotal:       {:>8} kB\nMemFree:        {:>8} kB\n",
            8 * 1024 * MIB,
            512 * MIB,
        );
        assert_eq!(memory(&meminfo).as_deref(), Some("512 MiB free of 8.0 GiB"));
        assert_eq!(
            memory("Buffers: 0 kB\n"),
            None,
            "and a file with neither number is unknown, not zero"
        );
    }
}
