//! What a core can be set to, asked of the core itself.
//!
//! A libretro core carries its own settings — PPSSPP's rendering resolution,
//! Mesen's overclock — and they are not in any file. The core *declares* them,
//! at run time, by handing the frontend a table during the first call it
//! receives: their keys, what to call them on screen, which category each
//! belongs to, every value each will take, and which value is the default.
//! RetroArch's own settings screens are drawn from that table and from nothing
//! else.
//!
//! Which leaves a shell wanting to offer those settings with one way of finding
//! out what they are: ask the core. So this loads the shared object, hands it a
//! callback, writes down what it says, and never runs it — no game, no window,
//! no `retro_init`. The table arrives during `retro_set_environment`, which is
//! the first thing a frontend calls and is documented to be safe before
//! anything else.
//!
//! A core that will not answer that way is reported with nothing, and that is
//! deliberate. There was a second, deeper ask here once — `retro_init` and then
//! `retro_load_game` on one of this machine's games — put in for Play!, which
//! declares its settings from inside `retro_load_game` and nothing before it.
//! It went out with Play!: `retro_init` alone segfaults PPSSPP under a frontend
//! this small, and running an emulator to fill in a settings page is a great
//! deal of machinery to carry for a core nothing defaults to.
//!
//! ## Why this is not a table in this repository
//!
//! Because it would be eighty-one tables, each of them a copy of something the
//! core already knows, going stale on the core's next release. PPSSPP alone
//! declares seventy-five options across five categories; a hand-written copy
//! would be wrong the first time somebody updated it and would say so nowhere.
//!
//! ## One line per core
//!
//! A record is written the moment a core has answered, rather than all of them
//! at the end. Loading an emulator into this process is the one thing this
//! binary does that somebody else's code decides the outcome of — a core is
//! free to do as it likes in a static initialiser — so a core that takes the
//! process down with it must not take the answers already collected with it.

use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::path::Path;
use std::sync::Mutex;

use crate::report::{CoreCategory, CoreOptions, CoreSetting, CoreValue, PROTOCOL};

/// `RTLD_LAZY | RTLD_LOCAL`: resolve as it goes, and keep the core's symbols
/// out of the global namespace so two cores cannot see each other's.
const RTLD_LAZY: c_int = 1;
const RTLD_LOCAL: c_int = 0;

unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

/// The environment calls this reads, by their numbers in `libretro.h`.
///
/// Written out rather than pulled from a binding crate for the reason the zip
/// reader is: what is needed is five numbers and four structure layouts, and
/// they have not changed since they were added.
const GET_CAN_DUPE: c_uint = 3;
const GET_SYSTEM_DIRECTORY: c_uint = 9;
const SET_PIXEL_FORMAT: c_uint = 10;
const GET_LOG_INTERFACE: c_uint = 27;
const GET_SAVE_DIRECTORY: c_uint = 31;
const GET_CORE_OPTIONS_VERSION: c_uint = 52;
const SET_VARIABLES: c_uint = 16;
const SET_CORE_OPTIONS: c_uint = 53;
const SET_CORE_OPTIONS_INTL: c_uint = 54;
const SET_CORE_OPTIONS_V2: c_uint = 67;
const SET_CORE_OPTIONS_V2_INTL: c_uint = 68;

/// The most values one option may have, which is `libretro.h`'s own ceiling.
const VALUES_MAX: usize = 128;

#[repr(C)]
struct RawValue {
    value: *const c_char,
    label: *const c_char,
}

#[repr(C)]
struct RawDefinitionV2 {
    key: *const c_char,
    desc: *const c_char,
    desc_categorized: *const c_char,
    info: *const c_char,
    info_categorized: *const c_char,
    category_key: *const c_char,
    values: [RawValue; VALUES_MAX],
    default_value: *const c_char,
}

#[repr(C)]
struct RawDefinitionV1 {
    key: *const c_char,
    desc: *const c_char,
    info: *const c_char,
    values: [RawValue; VALUES_MAX],
    default_value: *const c_char,
}

#[repr(C)]
struct RawCategory {
    key: *const c_char,
    desc: *const c_char,
    info: *const c_char,
}

#[repr(C)]
struct RawOptionsV2 {
    categories: *const RawCategory,
    definitions: *const RawDefinitionV2,
}

#[repr(C)]
struct RawOptionsV2Intl {
    us: *const RawOptionsV2,
    local: *const RawOptionsV2,
}

#[repr(C)]
struct RawOptionsV1Intl {
    us: *const RawDefinitionV1,
    local: *const RawDefinitionV1,
}

/// What `retro_get_system_info` fills in. Only the first field is read: it is
/// the name RetroArch files that core's chosen settings under, so it is the
/// name anything writing those settings has to spell them with.
#[repr(C)]
struct RawSystemInfo {
    library_name: *const c_char,
    library_version: *const c_char,
    valid_extensions: *const c_char,
    need_fullpath: bool,
    block_extract: bool,
}

#[repr(C)]
struct RawVariable {
    key: *const c_char,
    value: *const c_char,
}

// What the core said, while it is saying it.
//
// A C callback carries no state of its own, so what it collects has to live
// somewhere it can reach. Emptied before each core rather than after — a core
// that died half way leaves whatever it had said behind, and the next one must
// not inherit it.
//
// Shared rather than one thread's, and that is not caution. An emulator taken
// as far as `retro_init` has started threads of its own by the time it declares
// anything, and it is entitled to make the call from one of them: dolphin does.
// Held in a thread's own storage, that answer would arrive somewhere the run
// that asked for it cannot see, and the core would read as declaring nothing.
static HEARD: Mutex<CoreOptions> = Mutex::new(CoreOptions::empty());

/// What [`HEARD`] holds, taken briefly.
///
/// A helper rather than the lock written out nine times, and the one place the
/// poisoning rule is stated: a core that panicked a previous holder is exactly
/// the case this is for, so the contents are taken rather than the thread being
/// brought down after it.
fn heard<T>(with: impl FnOnce(&mut CoreOptions) -> T) -> T {
    let mut held = HEARD.lock().unwrap_or_else(|err| err.into_inner());
    with(&mut held)
}

/// libretro's `retro_log_callback`: one function pointer and nothing else.
#[repr(C)]
struct RawLog {
    log: unsafe extern "C" fn(c_int, *const c_char, ...),
}

/// The logger handed to a core that asks for one, which throws the line away.
///
/// Declared without the `...` and installed as though it had one. Defining a
/// variadic function is not something Rust does, and what this has to be is a
/// pointer a C caller may pass anything to — so it is written as the fixed part
/// alone and transmuted, which is sound on every ABI this ships for: the extra
/// arguments go in registers and on the stack the caller cleans up, and a
/// callee that reads none of them is a callee that returns.
///
/// It has to exist at all because refusing the request is what a core is least
/// prepared for. The libretro pattern is to take the frontend's logger *or*
/// fall back to its own, and a core that forgets the second half keeps a null
/// pointer and calls it: pcsx2 segfaults inside `retro_init` for exactly this
/// reason, which read for a long time as an emulator that could not be asked.
unsafe extern "C" fn swallow(_level: c_int, _format: *const c_char) {}

/// Where a core may look for the files it reads beside itself, and where it may
/// write.
///
/// Set once, before any core is handed anything, and never cleared: a core is
/// entitled to keep the pointer it is given — `libretro.h` says these strings
/// belong to the frontend and stay valid — so what is handed over has to
/// outlive every call the core will make, which for a `OnceLock` is the whole
/// process.
///
/// The save folder must not be RetroArch's. Asking an emulator what it can be
/// set to is not playing anything, and one taken as far as a game will lay down
/// a memory card or a whole `User` tree on its way past — dolphin does — which
/// has to land somewhere nobody will find it again. See `main.rs`, which hands
/// this a scratch directory.
static SYSTEM_AT: std::sync::OnceLock<CString> = std::sync::OnceLock::new();
static SAVE_AT: std::sync::OnceLock<CString> = std::sync::OnceLock::new();

/// Tell the probe where those two folders are, before it asks anything.
pub fn folders(system: Option<&Path>, save: &Path) {
    if let Some(system) = system {
        if let Ok(text) = CString::new(system.as_os_str().as_encoded_bytes()) {
            let _ = SYSTEM_AT.set(text);
        }
    }
    if let Ok(text) = CString::new(save.as_os_str().as_encoded_bytes()) {
        let _ = SAVE_AT.set(text);
    }
}

/// Hand a core one of those paths, or say this frontend has not got one.
///
/// # Safety
///
/// `data` is what the core passed for a call documented to take a
/// `const char **`, which is either null or writable.
unsafe fn hand(data: *mut c_void, at: Option<&'static CString>) -> bool {
    let Some(at) = at else { return false };
    if data.is_null() {
        return false;
    }
    unsafe { *data.cast::<*const c_char>() = at.as_ptr() };
    true
}

/// A borrowed C string, or `None` for null and for anything that is not UTF-8.
///
/// Not lossy, deliberately. These are identifiers and labels a core compiled
/// into itself; one that is not UTF-8 is one this has misread, and half a label
/// on a settings row is worse than no row.
///
/// # Safety
///
/// `at` is either null or a pointer to a null-terminated string that outlives
/// this call — which is what `libretro.h` requires of every string in these
/// tables: they are static storage belonging to the core.
unsafe fn text(at: *const c_char) -> Option<String> {
    if at.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(at) }
        .to_str()
        .ok()
        .map(str::to_string)
}

/// Which of the two names a V2 table gives a setting to put on its row.
///
/// libretro's newer table carries both: the full name, and a short one for when
/// the setting is already standing under its own group. pcsx2 calls one of them
/// "Emulation > EE Cycle Rate" and the other "EE Cycle Rate", and this shell
/// files that setting in a folder called Emulation — so the full name reads as
/// the group twice, once on the folder and once on every row inside it.
///
/// So the short one where the setting has a group to be shown under, and the
/// full one where it has not: an option a core sorted nowhere stands on the
/// page on its own, with nothing above it to say what it belongs to.
fn shown(full: Option<String>, categorised: Option<String>, grouped: bool, key: &str) -> String {
    let chosen = if grouped { categorised.or(full) } else { full };
    chosen.unwrap_or_else(|| key.to_string())
}

/// The values of one option, up to the ceiling and stopping at the first null.
///
/// # Safety
///
/// `values` is the core's own array, terminated by an entry with a null `value`.
unsafe fn values(values: &[RawValue; VALUES_MAX]) -> Vec<CoreValue> {
    let mut out = Vec::new();
    for raw in values.iter() {
        let Some(value) = (unsafe { text(raw.value) }) else {
            break;
        };
        out.push(CoreValue {
            // The label is what a person reads and the value is what RetroArch
            // stores; a core that gives no label means the value is already
            // readable — "disabled", "2x" — and is used as it stands.
            label: unsafe { text(raw.label) }.unwrap_or_else(|| value.clone()),
            value,
        });
    }
    out
}

/// # Safety
///
/// `at` is the core's null-terminated array of definitions, or null.
unsafe fn read_v2(at: *const RawOptionsV2) {
    if at.is_null() {
        return;
    }
    let options = unsafe { &*at };

    if !options.categories.is_null() {
        let mut walk = options.categories;
        loop {
            let raw = unsafe { &*walk };
            let Some(key) = (unsafe { text(raw.key) }) else {
                break;
            };
            let desc = unsafe { text(raw.desc) }.unwrap_or_else(|| key.clone());
            heard(|heard| heard.categories.push(CoreCategory { key, title: desc }));
            walk = unsafe { walk.add(1) };
        }
    }

    if options.definitions.is_null() {
        return;
    }
    let mut walk = options.definitions;
    loop {
        let raw = unsafe { &*walk };
        let Some(key) = (unsafe { text(raw.key) }) else {
            break;
        };
        let category = unsafe { text(raw.category_key) }.filter(|at| !at.is_empty());
        let title = shown(
            unsafe { text(raw.desc) },
            unsafe { text(raw.desc_categorized) }.filter(|at| !at.is_empty()),
            category.is_some(),
            &key,
        );
        let record = CoreSetting {
            key,
            title,
            note: unsafe { text(raw.info) },
            category,
            default: unsafe { text(raw.default_value) },
            values: unsafe { values(&raw.values) },
        };
        heard(|heard| heard.options.push(record));
        walk = unsafe { walk.add(1) };
    }
}

/// The same for the older table, which has no categories and no `info` split.
///
/// # Safety
///
/// `at` is the core's null-terminated array of definitions, or null.
unsafe fn read_v1(at: *const RawDefinitionV1) {
    if at.is_null() {
        return;
    }
    let mut walk = at;
    loop {
        let raw = unsafe { &*walk };
        let Some(key) = (unsafe { text(raw.key) }) else {
            break;
        };
        let title = unsafe { text(raw.desc) }.unwrap_or_else(|| key.clone());
        let record = CoreSetting {
            key,
            title,
            note: unsafe { text(raw.info) },
            category: None,
            default: unsafe { text(raw.default_value) },
            values: unsafe { values(&raw.values) },
        };
        heard(|heard| heard.options.push(record));
        walk = unsafe { walk.add(1) };
    }
}

/// The oldest table of all: a key and one string holding the label and every
/// value, `"Label; first|second|third"`.
///
/// # Safety
///
/// `at` is the core's null-terminated array of variables, or null.
unsafe fn read_variables(at: *const RawVariable) {
    if at.is_null() {
        return;
    }
    let mut walk = at;
    loop {
        let raw = unsafe { &*walk };
        let Some(key) = (unsafe { text(raw.key) }) else {
            break;
        };
        let said = unsafe { text(raw.value) }.unwrap_or_default();
        let (title, rest) = match said.split_once(';') {
            Some((title, rest)) => (title.trim().to_string(), rest.trim()),
            None => (key.clone(), said.as_str()),
        };
        let values: Vec<CoreValue> = rest
            .split('|')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| CoreValue {
                label: value.to_string(),
                value: value.to_string(),
            })
            .collect();
        let record = CoreSetting {
            key,
            title,
            note: None,
            category: None,
            // The first is the default in this form; the core says so nowhere
            // else.
            default: values.first().map(|first| first.value.clone()),
            values,
        };
        heard(|heard| heard.options.push(record));
        walk = unsafe { walk.add(1) };
    }
}

/// The frontend, as far as a core being asked what it can be set to is
/// concerned.
///
/// Everything except the option tables is refused — `false`, meaning "this
/// frontend does not do that" — which is a thing every core is required to cope
/// with, and is what keeps this from having to be a frontend.
///
/// # Safety
///
/// Called by the core with the command it is making and the data belonging to
/// it, which is the contract in `libretro.h`.
unsafe extern "C" fn environment(command: c_uint, data: *mut c_void) -> bool {
    match command {
        // The handful a core asks on its way to declaring anything. Refusing
        // them is what stops an emulator getting far enough to say what it can
        // be set to — see [`swallow`] for the one that is not merely unhelpful
        // but fatal.
        GET_LOG_INTERFACE => {
            if data.is_null() {
                return false;
            }
            // SAFETY: `data` is the core's own `retro_log_callback`, which is
            // one function pointer; and see [`swallow`] on the signature.
            unsafe {
                (*data.cast::<RawLog>()).log = std::mem::transmute::<
                    unsafe extern "C" fn(c_int, *const c_char),
                    unsafe extern "C" fn(c_int, *const c_char, ...),
                >(swallow)
            };
            true
        }
        GET_CAN_DUPE => {
            if data.is_null() {
                return false;
            }
            // SAFETY: documented to take a `bool *`.
            unsafe { *data.cast::<bool>() = true };
            true
        }
        // Nothing here draws a frame, so which format it is does not matter;
        // that it was allowed does.
        SET_PIXEL_FORMAT => true,
        GET_SYSTEM_DIRECTORY => unsafe { hand(data, SYSTEM_AT.get()) },
        GET_SAVE_DIRECTORY => unsafe { hand(data, SAVE_AT.get()) },
        // Answering 2 is what makes a core that has the newer table hand over
        // the newer table, which is the one with the categories in it.
        GET_CORE_OPTIONS_VERSION => {
            if data.is_null() {
                return false;
            }
            unsafe { *data.cast::<c_uint>() = 2 };
            true
        }
        SET_CORE_OPTIONS_V2 => {
            unsafe { read_v2(data.cast::<RawOptionsV2>()) };
            announce();
            true
        }
        // The translated table: the US half is the one with keys in it, and the
        // local half is the same table in somebody's language. The shell has
        // its own translations and does not want a second set here.
        SET_CORE_OPTIONS_V2_INTL => {
            if data.is_null() {
                return false;
            }
            let intl = unsafe { &*data.cast::<RawOptionsV2Intl>() };
            unsafe { read_v2(intl.us) };
            announce();
            true
        }
        SET_CORE_OPTIONS => {
            unsafe { read_v1(data.cast::<RawDefinitionV1>()) };
            announce();
            true
        }
        SET_CORE_OPTIONS_INTL => {
            if data.is_null() {
                return false;
            }
            let intl = unsafe { &*data.cast::<RawOptionsV1Intl>() };
            unsafe { read_v1(intl.us) };
            announce();
            true
        }
        SET_VARIABLES => {
            unsafe { read_variables(data.cast::<RawVariable>()) };
            announce();
            true
        }
        _ => false,
    }
}

/// What the core calls itself.
///
/// # Safety
///
/// `handle` came back non-null from `dlopen` on a libretro core.
unsafe fn library_name(handle: *mut c_void) -> Option<String> {
    let name = CString::new("retro_get_system_info").expect("a literal with no zero byte");
    let symbol = unsafe { dlsym(handle, name.as_ptr()) };
    if symbol.is_null() {
        return None;
    }
    // SAFETY: every libretro core exports this with this signature, and it is
    // documented to be callable before anything else and to only fill in the
    // structure it is handed.
    let get: unsafe extern "C" fn(*mut RawSystemInfo) = unsafe { std::mem::transmute(symbol) };
    let mut info = RawSystemInfo {
        library_name: std::ptr::null(),
        library_version: std::ptr::null(),
        valid_extensions: std::ptr::null(),
        need_fullpath: false,
        block_extract: false,
    };
    unsafe { get(&mut info) };
    unsafe { text(info.library_name) }
}

/// A core that has been loaded and handed a frontend, with what it calls
/// itself.
struct Opened {
    handle: *mut c_void,
    display: Option<String>,
}

/// One of a core's entry points, or `None` where it does not export it.
///
/// # Safety
///
/// `handle` came back non-null from `dlopen` on a libretro core.
unsafe fn entry(handle: *mut c_void, named: &str) -> Option<*mut c_void> {
    let name = CString::new(named).ok()?;
    let symbol = unsafe { dlsym(handle, name.as_ptr()) };
    (!symbol.is_null()).then_some(symbol)
}

/// Ask one core what it can be set to, without running it.
///
/// `Err` with a sentence for a core that will not load or will not say what it
/// is called. A core that loads and simply declares nothing is not an error —
/// it is an answer, and one [`deeper`] may be able to improve on.
pub fn of(core: &str, at: &Path) -> Result<CoreOptions, String> {
    let opened = open(at)?;
    said(core, opened.display)
}

/// The same of a core that would not answer when it was merely asked.
///
/// Two more rungs, taken only as far as they are needed: `retro_init`, and then
/// `retro_load_game` **with no game at all**. Both are calls into an emulator
/// rather than a question put to a shared object, which is why the caller runs
/// this in a process of its own — see `main.rs`.
///
/// Nothing of anybody's is opened. A core is handed a null game, which is the
/// call `libretro.h` defines for a frontend starting one with no content, and
/// what comes back is whatever the core declared on the way. That distinction
/// is the whole reason this exists in this shape: filling in a settings page
/// must not mean starting somebody's game behind them.
///
/// ## Why it announces rather than returns
///
/// Because the useful answer often arrives just before the crash. dolphin
/// declares ninety-nine settings from inside `retro_load_game(NULL)` and then
/// goes on to set up video, where a frontend this small has nothing for it and
/// it dies. Waiting for a clean return would throw away a complete table this
/// process was holding a moment earlier, so `say` is called the instant one
/// arrives and the caller keeps the newest thing it heard.
///
/// The game is never unloaded and the core is never deinitialised: both are
/// calls into an emulator that has just started threads, either can fail, and
/// what they would buy is tidiness in a process that is about to end anyway.
pub fn deeper(core: &str, at: &Path, say: fn(&CoreOptions)) {
    let Ok(opened) = open(at) else { return };
    let Some(display) = opened.display.clone() else {
        return;
    };
    *ANNOUNCE.lock().unwrap_or_else(|err| err.into_inner()) = Some(Announcing {
        core: core.to_string(),
        display,
        say,
    });

    // SAFETY: both symbols are exported by every libretro core with the
    // signatures its own header declares, and `retro_init` is documented to be
    // the call after `retro_set_environment` — which is where this is.
    if let Some(symbol) = unsafe { entry(opened.handle, "retro_init") } {
        let init: unsafe extern "C" fn() = unsafe { std::mem::transmute(symbol) };
        unsafe { init() };
    }
    // pcsx2 answers here, and stopping is the point: a core that has said what
    // it can be set to must not then be taken into `retro_load_game`, which is
    // one more emulator entry point that can take the process with it.
    if heard(|held| !held.options.is_empty()) {
        return;
    }

    if let Some(symbol) = unsafe { entry(opened.handle, "retro_load_game") } {
        let load: unsafe extern "C" fn(*const c_void) -> bool =
            unsafe { std::mem::transmute(symbol) };
        // SAFETY: a null `retro_game_info` is what `libretro.h` defines for a
        // frontend starting a core with no content, and is the one argument
        // this may pass without opening anything of anybody's.
        let _ = unsafe { load(std::ptr::null()) };
    }
}

/// Where a table goes the moment it arrives, while [`deeper`] is running.
struct Announcing {
    core: String,
    display: String,
    /// A plain function pointer rather than a closure: what reaches this is a
    /// C callback with no state of its own, so there is nowhere for a captured
    /// environment to live.
    say: fn(&CoreOptions),
}

static ANNOUNCE: Mutex<Option<Announcing>> = Mutex::new(None);

/// Write out what has been declared so far, if this run is the deeper ask.
///
/// Called from the environment callback rather than after the entry point
/// returns, and that is the whole of why the deeper ask works on dolphin: it
/// declares its ninety-nine settings from inside `retro_load_game` and then
/// carries on into video setup and dies there. By the time the call returns
/// there is no process left to return into, so the answer has to leave the
/// building the moment it arrives.
///
/// Takes [`ANNOUNCE`] and then [`HEARD`], and nothing anywhere takes them the
/// other way round — the arms that call this have let go of `HEARD` first.
fn announce() {
    let held = ANNOUNCE.lock().unwrap_or_else(|err| err.into_inner());
    let Some(announcing) = held.as_ref() else {
        return;
    };
    let record = heard(|held| held.clone());
    // A core is entitled to declare its categories and its settings in two
    // calls. Nothing is worth saying until there is something to set.
    if record.options.is_empty() {
        return;
    }
    if let Ok(record) = ready(&announcing.core, Some(announcing.display.clone()), record) {
        (announcing.say)(&record);
    }
}

/// Load a core and hand it a frontend, which is where most of them answer.
fn open(at: &Path) -> Result<Opened, String> {
    let path = CString::new(at.as_os_str().as_encoded_bytes())
        .map_err(|_| "the path has a zero byte in it".to_string())?;

    // SAFETY: the path is a valid C string and the flags are the two documented
    // constants. What comes back is checked for null before it is used.
    let handle = unsafe { dlopen(path.as_ptr(), RTLD_LAZY | RTLD_LOCAL) };
    if handle.is_null() {
        // SAFETY: `dlerror` returns a static string or null, and is only
        // meaningful straight after a failed call — which is where this is.
        let why = unsafe { text(dlerror()) }.unwrap_or_else(|| "it would not load".to_string());
        return Err(why);
    }

    let name = CString::new("retro_set_environment").expect("a literal with no zero byte");
    // SAFETY: the handle came back non-null from `dlopen` above.
    let symbol = unsafe { dlsym(handle, name.as_ptr()) };
    if symbol.is_null() {
        return Err("it is not a libretro core".to_string());
    }
    // SAFETY: every libretro core exports this symbol with this signature; a
    // shared object that exports it with another is not one, and there is no
    // way to tell from here — which is the same position RetroArch is in.
    let set_environment: unsafe extern "C" fn(unsafe extern "C" fn(c_uint, *mut c_void) -> bool) =
        unsafe { std::mem::transmute(symbol) };

    // What the core calls itself, which is the folder RetroArch keeps its
    // settings in. Asked before the options because it costs nothing and
    // because a core whose settings cannot be filed anywhere is one there is
    // no point offering settings for.
    let display = unsafe { library_name(handle) };

    heard(|heard| *heard = CoreOptions::empty());
    // SAFETY: handing the core a function pointer of the signature its own
    // header declares. This is the first call a frontend makes and the one
    // documented to carry these tables.
    unsafe { set_environment(environment) };

    Ok(Opened { handle, display })
}

/// What one core said, made into the record the shell reads.
fn said(core: &str, display: Option<String>) -> Result<CoreOptions, String> {
    let record = heard(|held| std::mem::replace(held, CoreOptions::empty()));
    ready(core, display, record)
}

/// The same, of a record already in hand.
///
/// Split out for [`deeper`], which reads what has been said so far *without*
/// emptying the store: a core is entitled to declare twice, and the second
/// table has to arrive on top of the first rather than instead of it.
fn ready(core: &str, display: Option<String>, record: CoreOptions) -> Result<CoreOptions, String> {
    let mut record = record;
    // A core that said nothing is still reported, with nothing in it. Answering
    // `Err` for that put the core in the log and nowhere else, so somebody who
    // had just installed one went looking under Settings and found the page it
    // plays for missing altogether — which reads as the install having failed.
    record.protocol = PROTOCOL;
    record.core = core.to_string();
    let Some(display) = display else {
        return Err("it will not say what it is called".to_string());
    };
    record.display = display;
    // An option nobody can choose between is not a row: a core may declare one
    // whose values it worked out at run time and found none of.
    record.options.retain(|option| option.values.len() > 1);
    // The handle is deliberately *not* closed. A core is free to leave threads
    // and atexit handlers behind it, and unloading one that has is how a
    // process dies after the useful work is done. This one exits in a moment
    // anyway.
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(key: &str, values: &[&str]) -> CoreSetting {
        CoreSetting {
            key: key.to_string(),
            title: key.to_string(),
            note: None,
            category: None,
            default: values.first().map(|value| (*value).to_string()),
            values: values
                .iter()
                .map(|value| CoreValue {
                    value: (*value).to_string(),
                    label: (*value).to_string(),
                })
                .collect(),
        }
    }

    fn held(options: Vec<CoreSetting>) -> CoreOptions {
        CoreOptions {
            options,
            ..CoreOptions::empty()
        }
    }

    /// A setting nobody can choose between is not a row.
    ///
    /// Cores work some of their values out at run time — which BIOS images are
    /// on this machine, which renderers this GPU has — and one that found a
    /// single answer has nothing to ask about. This is why the counts the shell
    /// shows are one or two short of the lines in RetroArch's own `.opt` file,
    /// which is a thing worth being able to look up rather than rediscover.
    #[test]
    fn a_setting_with_one_value_is_not_a_row() {
        let record = ready(
            "pcsx2",
            Some("LRPS2".to_string()),
            held(vec![
                setting("pcsx2_bios", &["only-one.bin"]),
                setting("pcsx2_renderer", &["Auto", "Vulkan"]),
                setting("pcsx2_nothing", &[]),
            ]),
        )
        .expect("a core that said what it is called");

        assert_eq!(
            record
                .options
                .iter()
                .map(|option| option.key.as_str())
                .collect::<Vec<_>>(),
            ["pcsx2_renderer"],
            "the one with a choice in it, and only that one"
        );
        assert_eq!(record.core, "pcsx2");
        assert_eq!(record.display, "LRPS2");
        assert_eq!(record.protocol, PROTOCOL);
    }

    /// A core that will not say what it is called gets no page.
    ///
    /// The name is the folder RetroArch files that core's chosen settings
    /// under, so a page built without one would write somebody's answers where
    /// nothing would ever read them back.
    #[test]
    fn a_core_that_will_not_name_itself_is_an_error() {
        assert!(ready("pcsx2", None, held(vec![setting("a", &["1", "2"])])).is_err());
    }

    /// Everything the globals below are stated by, one test at a time.
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    /// Where the fake `say` puts what it was handed.
    static WROTE: Mutex<Vec<CoreOptions>> = Mutex::new(Vec::new());

    fn wrote(record: &CoreOptions) {
        WROTE
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .push(record.clone());
    }

    /// A core that declares twice is written out whole the second time.
    ///
    /// The deeper ask announces from inside the core's own callback, because
    /// the answer often arrives moments before the core dies — and a core is
    /// entitled to declare in two calls, its variables in one and its richer
    /// table in the next. If announcing *took* what it had read, the second
    /// line would carry only the second half, and the caller keeps the last
    /// line it saw. The whole table would be lost to the very mechanism that
    /// exists to save it.
    #[test]
    fn a_core_that_declares_twice_is_written_out_whole() {
        let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|err| err.into_inner());
        heard(|held| *held = CoreOptions::empty());
        WROTE.lock().unwrap_or_else(|err| err.into_inner()).clear();
        *ANNOUNCE.lock().unwrap_or_else(|err| err.into_inner()) = Some(Announcing {
            core: "twice".to_string(),
            display: "Twice".to_string(),
            say: wrote,
        });

        heard(|held| held.options.push(setting("first", &["1", "2"])));
        announce();
        heard(|held| held.options.push(setting("second", &["1", "2"])));
        announce();

        let wrote = WROTE.lock().unwrap_or_else(|err| err.into_inner()).clone();
        assert_eq!(wrote.len(), 2, "one line per declaration");
        assert_eq!(wrote[0].options.len(), 1);
        assert_eq!(
            wrote[1]
                .options
                .iter()
                .map(|option| option.key.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"],
            "the last line carries the whole table and not just the new half"
        );

        *ANNOUNCE.lock().unwrap_or_else(|err| err.into_inner()) = None;
        heard(|held| *held = CoreOptions::empty());
    }

    /// A setting under a group wears the short name, and one on its own wears
    /// the full one.
    ///
    /// The row would otherwise read "Emulation > EE Cycle Rate" inside a folder
    /// called Emulation, which is the group said twice.
    #[test]
    fn a_setting_under_a_group_is_not_named_after_it_as_well() {
        let full = || Some("Emulation > EE Cycle Rate".to_string());
        let short = || Some("EE Cycle Rate".to_string());

        assert_eq!(shown(full(), short(), true, "k"), "EE Cycle Rate");
        assert_eq!(
            shown(full(), short(), false, "k"),
            "Emulation > EE Cycle Rate",
            "sorted nowhere, it stands on its own and says what it belongs to"
        );
        // An older core gives only the one name, and a grouped setting has to
        // fall back to it rather than to the key.
        assert_eq!(shown(full(), None, true, "k"), "Emulation > EE Cycle Rate");
        // And a core that gives neither leaves the key, which is at least
        // something somebody can look up.
        assert_eq!(
            shown(None, None, true, "pcsx2_ee_cycle_rate"),
            "pcsx2_ee_cycle_rate"
        );
    }

    /// Nothing is written before there is something to set.
    ///
    /// A core that has declared its groups and no settings yet has said nothing
    /// worth a line, and a line with an empty table in it is one the caller
    /// would keep as the newest answer.
    #[test]
    fn a_table_with_nothing_in_it_is_not_announced() {
        let _held = ONE_AT_A_TIME.lock().unwrap_or_else(|err| err.into_inner());
        heard(|held| *held = CoreOptions::empty());
        WROTE.lock().unwrap_or_else(|err| err.into_inner()).clear();
        *ANNOUNCE.lock().unwrap_or_else(|err| err.into_inner()) = Some(Announcing {
            core: "empty".to_string(),
            display: "Empty".to_string(),
            say: wrote,
        });

        heard(|held| {
            held.categories.push(CoreCategory {
                key: "video".to_string(),
                title: "Video".to_string(),
            })
        });
        announce();
        assert!(
            WROTE
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .is_empty(),
            "groups alone are not an answer"
        );

        *ANNOUNCE.lock().unwrap_or_else(|err| err.into_inner()) = None;
        heard(|held| *held = CoreOptions::empty());
    }
}
