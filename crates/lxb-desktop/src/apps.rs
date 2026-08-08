//! Application discovery: XDG desktop entries, grouped into Plasma-style
//! categories.
//!
//! The `.desktop` format is a small INI dialect, and the parts we need (the
//! `Desktop Entry` group, localised names, `Exec` field codes) are stable and
//! well specified, so it is parsed here rather than pulled in as a dependency.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::theme::Color;

/// A launchable application.
#[derive(Debug, Clone)]
pub struct App {
    pub name: String,
    pub comment: Option<String>,
    pub icon: Option<String>,
    /// `Exec` with field codes already stripped.
    pub exec: String,
    pub terminal: bool,
    pub categories: Vec<String>,
    pub path: PathBuf,
    /// `StartupWMClass`: what this application's windows will call themselves,
    /// stated by the application itself. Only 16 of the 235 entries installed
    /// on the machine this was written on set it, so it is the best answer
    /// rather than the only one — see [`App::window_names`].
    pub wm_class: Option<String>,
}

impl App {
    /// Every name a window of this application might go by, best first.
    ///
    /// Asked before starting anything, to find out whether this application is
    /// already running. There is no registry mapping a desktop entry to the
    /// name its windows use, so this is the conventional guess every desktop
    /// makes: what the entry declares, then the entry's own file name (which
    /// is what a well-behaved application derives its app_id from), then the
    /// program it runs.
    pub fn window_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        let mut add = |name: Option<String>| {
            if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
                let known = |seen: &String| seen.eq_ignore_ascii_case(&name);
                if !names.iter().any(known) {
                    names.push(name);
                }
            }
        };

        add(self.wm_class.clone());
        add(self
            .path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string));
        add(program_name(&self.exec));
        names
    }

    /// Whether a window calling itself `app_id` is one of this application's.
    ///
    /// Loose in two directions, because the two sides spell the same
    /// application differently: case, since an X11 class is conventionally
    /// capitalised (`Steam`) where a desktop entry is not, and the trailing
    /// component of a reverse-DNS name, since an application shipped as
    /// `org.mozilla.firefox` still runs `firefox`. Both are what every other
    /// desktop matches on, and the cost of being wrong is bounded: the user
    /// gets the window they already had instead of a second copy.
    pub fn owns_window(&self, app_id: &str) -> bool {
        let app_id = app_id.trim();
        if app_id.is_empty() {
            return false;
        }
        let tail = |name: &str| {
            name.rsplit('.')
                .next()
                .filter(|tail| !tail.is_empty())
                .unwrap_or(name)
                .to_string()
        };
        self.window_names().iter().any(|name| {
            name.eq_ignore_ascii_case(app_id) || tail(name).eq_ignore_ascii_case(&tail(app_id))
        })
    }
}

/// The name of the program an `Exec` line runs, without its path or arguments.
fn program_name(exec: &str) -> Option<String> {
    let program = exec.split_whitespace().next()?;
    // An `Exec` that starts with an environment wrapper names the program
    // further along; the wrapper is nobody's window name.
    let program = match program.rsplit('/').next()? {
        "env" | "sh" | "bash" | "flatpak" => exec.split_whitespace().nth(1)?,
        _ => program,
    };
    program
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty() && !name.starts_with('-'))
        .map(str::to_string)
}

/// One row of a column.
///
/// A column is a tree rather than a list, as the original cross media bar's
/// were: a row is something to launch, a subcategory holding a column of its
/// own, or one of a set of values the shell is set to. One type rather than
/// three, because a row is drawn the same way whichever it is — an icon, a
/// title, and a line under it — and the bar's whole job is that they all sit
/// in one column together.
#[derive(Debug, Clone)]
pub enum Entry {
    App(App),
    Folder(Folder),
    Choice(Choice),
}

/// A subcategory: a column of its own, stepped into from the row that names it.
#[derive(Debug, Clone)]
pub struct Folder {
    pub title: String,
    pub comment: Option<String>,
    /// One of the shell's own glyphs, and looked up as one — without the
    /// missing-icon fallback an application gets. A subcategory that came out
    /// as the generic executable icon would read as an application that cannot
    /// be launched rather than as a way further in.
    pub icon: Option<String>,
    pub entries: Vec<Entry>,
}

/// One of a set of alternatives, exactly one of which is in force.
///
/// The row a settings list is made of. It is a leaf: choosing it changes what
/// the shell is set to rather than opening anything.
#[derive(Debug, Clone)]
pub struct Choice {
    pub title: String,
    pub comment: Option<String>,
    pub icon: Option<String>,
    /// What this row stands for, when the setting is a colour. The atlas
    /// multiplies a quad's colour into the texel it samples, so a plain white
    /// swatch drawn in this comes out as the colour itself — which is the one
    /// label a colour cannot be given in words.
    pub swatch: Option<Color>,
    /// Whether this is the one currently in force.
    pub chosen: bool,
    /// What choosing this row does. `None` for a value the shell can show but
    /// not change, which stays inert rather than taking the mark off a row
    /// that describes something true.
    pub setting: Option<crate::settings::Setting>,
}

/// A top-level XMB column.
#[derive(Debug, Clone)]
pub struct Category {
    pub id: &'static str,
    pub title: &'static str,
    /// Icon name looked up in the icon theme.
    pub icon: &'static str,
    pub entries: Vec<Entry>,
}

/// The shell's own column, for settings that belong to LineXinBar itself rather
/// than to anything installed on the system.
///
/// Always present and always first, the way the real XMB opens on Settings.
/// It is deliberately not part of [`CATEGORY_TABLE`]: nothing on disk is
/// classified into it, so it is not a destination for `.desktop` files.
pub const SHELL_SETTINGS: (&str, &str, &str) =
    ("settings", "Settings", crate::icons::CATEGORY_SETTINGS);

/// Where installed applications go, in XMB order.
///
/// Each entry lists the XDG main categories that map onto it, and the first
/// match wins. `Settings` and `System` share a column, as they do in Plasma —
/// its menu has no Settings menu of its own, and the shell's own Settings
/// column is not somewhere an installed application belongs.
const CATEGORY_TABLE: &[(&str, &str, &str, &[&str])] = &[
    (
        "system",
        "System",
        crate::icons::CATEGORY_SYSTEM,
        &["Settings", "System"],
    ),
    (
        "multimedia",
        "Multimedia",
        crate::icons::CATEGORY_MULTIMEDIA,
        &["AudioVideo", "Audio", "Video"],
    ),
    (
        "graphics",
        "Graphics",
        crate::icons::CATEGORY_GRAPHICS,
        &["Graphics"],
    ),
    (
        "internet",
        "Internet",
        crate::icons::CATEGORY_INTERNET,
        &["Network"],
    ),
    (
        "office",
        "Office",
        crate::icons::CATEGORY_OFFICE,
        &["Office"],
    ),
    ("games", "Games", crate::icons::CATEGORY_GAMES, &["Game"]),
    (
        "development",
        "Development",
        crate::icons::CATEGORY_DEVELOPMENT,
        &["Development"],
    ),
    (
        "education",
        "Education & Science",
        crate::icons::CATEGORY_EDUCATION,
        &["Education", "Science"],
    ),
    (
        "utilities",
        "Utilities",
        crate::icons::CATEGORY_UTILITIES,
        &["Utility"],
    ),
    ("other", "Other", crate::icons::CATEGORY_OTHER, &[]),
];

impl App {
    /// Parse one `.desktop` file. Returns `None` for entries that should not
    /// appear in a menu (hidden, `NoDisplay`, non-application types).
    pub fn from_file(path: &Path) -> Option<App> {
        let raw = std::fs::read_to_string(path).ok()?;
        Self::parse(&raw, path)
    }

    fn parse(raw: &str, path: &Path) -> Option<App> {
        let mut in_entry = false;
        let mut fields: BTreeMap<String, String> = BTreeMap::new();

        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.starts_with('[') {
                // Only the main group matters; actions and other groups are skipped.
                in_entry = line == "[Desktop Entry]";
                continue;
            }
            if !in_entry {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                fields
                    .entry(key.trim().to_string())
                    .or_insert_with(|| value.trim().to_string());
            }
        }

        if fields.get("Type").map(String::as_str) != Some("Application") {
            return None;
        }
        if is_true(fields.get("NoDisplay")) || is_true(fields.get("Hidden")) {
            return None;
        }

        let name = localised(&fields, "Name")?;
        let exec = strip_field_codes(fields.get("Exec")?);
        if exec.trim().is_empty() {
            return None;
        }

        if !shown_in(&fields, &current_desktops()) {
            return None;
        }

        let categories = fields
            .get("Categories")
            .map(|c| {
                c.split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        Some(App {
            name,
            comment: localised(&fields, "Comment"),
            icon: fields.get("Icon").cloned(),
            exec,
            terminal: is_true(fields.get("Terminal")),
            categories,
            path: path.to_path_buf(),
            wm_class: fields.get("StartupWMClass").cloned(),
        })
    }

    /// Which column this app belongs in.
    fn category_id(&self) -> &'static str {
        for (id, _, _, xdg) in CATEGORY_TABLE {
            if xdg
                .iter()
                .any(|c| self.categories.iter().any(|own| own == c))
            {
                return id;
            }
        }
        "other"
    }
}

impl Entry {
    /// What the row is called.
    pub fn title(&self) -> &str {
        match self {
            Entry::App(app) => &app.name,
            Entry::Folder(folder) => &folder.title,
            Entry::Choice(choice) => &choice.title,
        }
    }

    /// The line under the title, when there is one to say.
    pub fn comment(&self) -> Option<&str> {
        match self {
            Entry::App(app) => app.comment.as_deref(),
            Entry::Folder(folder) => folder.comment.as_deref(),
            Entry::Choice(choice) => choice.comment.as_deref(),
        }
    }

    pub fn icon(&self) -> Option<&str> {
        match self {
            Entry::App(app) => app.icon.as_deref(),
            Entry::Folder(folder) => folder.icon.as_deref(),
            Entry::Choice(choice) => choice.icon.as_deref(),
        }
    }

    /// The column this row opens into, if it opens into one.
    pub fn entries(&self) -> Option<&[Entry]> {
        match self {
            Entry::Folder(folder) => Some(&folder.entries),
            _ => None,
        }
    }

    /// The same column, to be changed: the shell's own rows hold state — which
    /// value is in force — and moving that mark means writing to the tree the
    /// bar is drawn from.
    pub fn entries_mut(&mut self) -> Option<&mut [Entry]> {
        match self {
            Entry::Folder(folder) => Some(&mut folder.entries),
            _ => None,
        }
    }

    /// What this row would launch, if launching is what it does.
    pub fn app(&self) -> Option<&App> {
        match self {
            Entry::App(app) => Some(app),
            _ => None,
        }
    }

    /// The colour this row stands for — see [`Choice::swatch`].
    pub fn swatch(&self) -> Option<Color> {
        match self {
            Entry::Choice(choice) => choice.swatch,
            _ => None,
        }
    }

    /// Whether this row is the value its column is currently set to.
    pub fn chosen(&self) -> bool {
        matches!(self, Entry::Choice(choice) if choice.chosen)
    }

    /// The setting this value would apply, if it is an editable value.
    pub fn setting(&self) -> Option<crate::settings::Setting> {
        match self {
            Entry::Choice(choice) => choice.setting,
            _ => None,
        }
    }
}

/// Hand every entry in `entries` to `visit`, including those inside
/// subcategories.
///
/// The bar's columns are trees, so anything that has to see all of them — the
/// icon atlas being filled, an application being counted — has to walk rather
/// than iterate.
pub fn walk(entries: &[Entry], visit: &mut impl FnMut(&Entry)) {
    for entry in entries {
        visit(entry);
        if let Some(children) = entry.entries() {
            walk(children, visit);
        }
    }
}

impl Category {
    /// Whether there is an application anywhere in this column, subcategories
    /// included.
    ///
    /// Not the same as having no rows: the shell's own Settings column is full
    /// of rows and holds nothing that can be launched.
    pub fn has_app(&self) -> bool {
        let mut found = false;
        walk(&self.entries, &mut |entry| found |= entry.app().is_some());
        found
    }

    /// How many applications this column holds, subcategories included.
    pub fn apps(&self) -> usize {
        let mut count = 0;
        walk(&self.entries, &mut |entry| {
            count += usize::from(entry.app().is_some())
        });
        count
    }

    /// What to say when this column has nothing in it.
    ///
    /// Only ever seen in the shell's own column, since a scanned one with no
    /// applications in it is dropped rather than drawn.
    pub fn empty_note(&self) -> &'static str {
        if self.id == SHELL_SETTINGS.0 {
            "LineXinBar's own settings will live here"
        } else {
            "No applications in this category"
        }
    }
}

fn is_true(value: Option<&String>) -> bool {
    value
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Whether `desktops` — the environments this session claims to be — allow an
/// entry to appear in a menu.
///
/// `OnlyShowIn` restricts an entry to the desktops it names and `NotShowIn`
/// bars it from them; both are `;`-separated lists, matched against the
/// `:`-separated names in `XDG_CURRENT_DESKTOP`. An entry naming neither is
/// shown everywhere.
///
/// Comparison ignores case. The spec's registered names are upper case by
/// convention rather than by rule, and entries in the wild are written both
/// ways for the same desktop.
fn shown_in(fields: &BTreeMap<String, String>, desktops: &[String]) -> bool {
    let names_this_session = |value: &String| {
        value
            .split(';')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .any(|name| desktops.iter().any(|ours| ours.eq_ignore_ascii_case(name)))
    };

    if fields
        .get("OnlyShowIn")
        .is_some_and(|v| !names_this_session(v))
    {
        return false;
    }
    !fields.get("NotShowIn").is_some_and(names_this_session)
}

/// The desktop names this session answers to.
///
/// LineXinBar's session sets `XDG_CURRENT_DESKTOP=LineXinBar`, and so does the
/// compositor for everything it launches, so the shell sees the same identity
/// nested as it does on its own. Nothing further is claimed on its behalf: an
/// entry written for one specific other desktop is written for that desktop's
/// session, not for this one.
fn current_desktops() -> Vec<String> {
    std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// Prefer a plain `Name`; localisation is left to the user's locale only when
/// an exact match exists, since partial matching tends to pick the wrong one.
fn localised(fields: &BTreeMap<String, String>, key: &str) -> Option<String> {
    if let Some(locale) = current_locale() {
        if let Some(value) = fields.get(&format!("{key}[{locale}]")) {
            return Some(value.clone());
        }
        // `pt_BR` also matches a bare `pt` entry.
        if let Some((lang, _)) = locale.split_once('_') {
            if let Some(value) = fields.get(&format!("{key}[{lang}]")) {
                return Some(value.clone());
            }
        }
    }
    fields.get(key).cloned()
}

fn current_locale() -> Option<String> {
    for var in ["LC_MESSAGES", "LC_ALL", "LANG"] {
        if let Ok(value) = std::env::var(var) {
            let value = value.split('.').next().unwrap_or("").to_string();
            if !value.is_empty() && value != "C" && value != "POSIX" {
                return Some(value);
            }
        }
    }
    None
}

/// Remove `%f`, `%U`, ... from an `Exec` line.
///
/// We launch applications with no arguments, so every field code expands to
/// nothing. `%%` is an escaped literal percent.
fn strip_field_codes(exec: &str) -> String {
    let mut out = String::with_capacity(exec.len());
    let mut chars = exec.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            // Known codes all expand to nothing for an argument-less launch.
            Some('f' | 'F' | 'u' | 'U' | 'd' | 'D' | 'n' | 'N' | 'i' | 'c' | 'k' | 'v' | 'm') => {}
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }

    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Scan the system for applications and group them into columns.
///
/// Entries earlier in the search path win, so a user's override in
/// `~/.local/share/applications` replaces the system copy of the same id.
pub fn scan() -> Vec<Category> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut apps: Vec<App> = Vec::new();

    for dir in crate::xdg_data_dirs("applications") {
        collect_from_dir(&dir, &dir, &mut seen, &mut apps);
    }

    assemble(apps)
}

/// Sort discovered applications into the bar's columns.
///
/// Split from [`scan`] so the arrangement can be exercised without a
/// filesystem to arrange.
fn assemble(apps: Vec<App>) -> Vec<Category> {
    let (id, title, icon) = SHELL_SETTINGS;
    let shell_settings = Category {
        id,
        title,
        icon,
        entries: crate::settings::column(),
    };

    let mut sorted: Vec<Vec<App>> = CATEGORY_TABLE.iter().map(|_| Vec::new()).collect();
    for app in apps {
        let id = app.category_id();
        if let Some(index) = CATEGORY_TABLE.iter().position(|(own, ..)| *own == id) {
            sorted[index].push(app);
        }
    }

    let mut categories: Vec<Category> = CATEGORY_TABLE
        .iter()
        .zip(&mut sorted)
        .map(|((id, title, icon, _), apps)| {
            apps.sort_by_key(|a| a.name.to_lowercase());
            Category {
                id,
                title,
                icon,
                entries: apps.drain(..).map(Entry::App).collect(),
            }
        })
        .collect();

    // Empty columns would just be dead space to scroll past. The shell's own
    // is exempt: it is a fixed part of the bar rather than a consequence of
    // what happens to be installed, and it has its own rows in it.
    categories.retain(|c| !c.entries.is_empty());
    categories.insert(0, shell_settings);
    categories
}

/// Recurse into a directory, tracking the desktop-file id so duplicates across
/// search paths collapse to one entry.
fn collect_from_dir(root: &Path, dir: &Path, seen: &mut HashSet<String>, apps: &mut Vec<App>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_from_dir(root, &path, seen, apps);
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
            continue;
        }

        // The id is the path below the search root, with `/` turned into `-`.
        let id = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('/', "-");
        if !seen.insert(id) {
            continue;
        }

        if let Some(app) = App::from_file(&path) {
            apps.push(app);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(raw: &str) -> Option<App> {
        App::parse(raw, Path::new("/tmp/test.desktop"))
    }

    #[test]
    fn parses_a_basic_entry() {
        let app = parse(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Text Editor\n\
             Comment=Edit text\n\
             Exec=gedit %U\n\
             Icon=accessories-text-editor\n\
             Categories=Utility;TextEditor;\n",
        )
        .unwrap();

        assert_eq!(app.name, "Text Editor");
        assert_eq!(app.exec, "gedit");
        assert_eq!(app.icon.as_deref(), Some("accessories-text-editor"));
        assert_eq!(app.category_id(), "utilities");
    }

    /// The real entries, as installed on the machine this was written for.
    /// Pressing any of these tiles while the application is running has to
    /// find the window rather than start a second copy.
    #[test]
    fn a_running_window_is_recognised_from_its_desktop_entry() {
        // Firefox declares the answer outright.
        let firefox = App::parse(
            "[Desktop Entry]\nType=Application\nName=Firefox\n\
             Exec=/usr/lib/firefox/firefox %u\nStartupWMClass=firefox\n",
            Path::new("/usr/share/applications/firefox.desktop"),
        )
        .unwrap();
        assert!(firefox.owns_window("firefox"));
        // Flatpaks of the same application name themselves in reverse DNS.
        assert!(firefox.owns_window("org.mozilla.firefox"));
        assert!(!firefox.owns_window("chromium"));

        // Steam declares nothing, and its X11 class is capitalised where the
        // entry is not.
        let steam = App::parse(
            "[Desktop Entry]\nType=Application\nName=Steam\nExec=/usr/bin/steam %U\n",
            Path::new("/usr/share/applications/steam.desktop"),
        )
        .unwrap();
        assert!(steam.owns_window("Steam"));
        assert!(steam.owns_window("steam"));

        // And an entry whose file name says nothing is still matched by the
        // program it runs.
        let dolphin = App::parse(
            "[Desktop Entry]\nType=Application\nName=Files\nExec=dolphin %u\n",
            Path::new("/usr/share/applications/org.kde.dolphin.desktop"),
        )
        .unwrap();
        assert!(dolphin.owns_window("org.kde.dolphin"));
        assert!(dolphin.owns_window("dolphin"));
    }

    /// The other direction, which is the one that costs the user something: a
    /// tile that matched the wrong window would refuse to start the
    /// application and raise somebody else's instead.
    #[test]
    fn an_unrelated_window_is_not_this_application() {
        let app = App::parse(
            "[Desktop Entry]\nType=Application\nName=Text Editor\nExec=gedit %U\n",
            Path::new("/usr/share/applications/gedit.desktop"),
        )
        .unwrap();
        assert!(!app.owns_window("kate"));
        assert!(!app.owns_window("org.gnome.TextEditor"));
        // A window whose client named itself nothing is evidence of nothing.
        assert!(!app.owns_window(""));
        assert!(!app.owns_window("   "));
    }

    /// A wrapper is not a window name: every flatpak would otherwise be the
    /// same application, and everything started through a shell would be
    /// `sh`.
    #[test]
    fn the_program_behind_a_wrapper_is_what_counts() {
        let flatpak = App::parse(
            "[Desktop Entry]\nType=Application\nName=Zen\n\
             Exec=flatpak run app.zen_browser.zen @@u %u @@\n",
            Path::new("/tmp/app.zen_browser.zen.desktop"),
        )
        .unwrap();
        assert!(flatpak.owns_window("app.zen_browser.zen"));
        assert!(!flatpak.owns_window("flatpak"));

        let wrapped = App::parse(
            "[Desktop Entry]\nType=Application\nName=Thing\nExec=env FOO=1 thing\n",
            Path::new("/tmp/thing-entry.desktop"),
        )
        .unwrap();
        assert!(!wrapped.owns_window("env"));
    }

    #[test]
    fn skips_hidden_and_non_applications() {
        assert!(
            parse("[Desktop Entry]\nType=Application\nName=X\nExec=x\nNoDisplay=true\n").is_none()
        );
        assert!(
            parse("[Desktop Entry]\nType=Application\nName=X\nExec=x\nHidden=true\n").is_none()
        );
        assert!(parse("[Desktop Entry]\nType=Link\nName=X\nURL=http://x\n").is_none());
        assert!(parse("[Desktop Entry]\nType=Application\nName=X\n").is_none());
    }

    fn fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn desktop_scoping_is_matched_rather_than_assumed() {
        let ours = ["LineXinBar".to_string()];

        // An entry naming this desktop is ours to show, whichever way round it
        // is written, and whatever else it lists alongside.
        assert!(shown_in(&fields(&[("OnlyShowIn", "LineXinBar;")]), &ours));
        assert!(shown_in(
            &fields(&[("OnlyShowIn", "KDE;linexinbar;")]),
            &ours
        ));
        assert!(shown_in(&fields(&[("NotShowIn", "KDE;GNOME;")]), &ours));

        // And one written for somebody else's session is not.
        assert!(!shown_in(&fields(&[("OnlyShowIn", "KDE;")]), &ours));
        assert!(!shown_in(&fields(&[("NotShowIn", "LineXinBar;")]), &ours));

        // Both keys at once: each has to be satisfied.
        let both = fields(&[("OnlyShowIn", "LineXinBar;"), ("NotShowIn", "LineXinBar;")]);
        assert!(!shown_in(&both, &ours));

        // An empty list names no desktop, so it can only exclude.
        assert!(!shown_in(&fields(&[("OnlyShowIn", "")]), &ours));
        assert!(shown_in(&fields(&[("NotShowIn", "")]), &ours));

        // Saying nothing means everywhere, including a session that has no
        // identity at all to match against.
        assert!(shown_in(&fields(&[]), &ours));
        assert!(shown_in(&fields(&[]), &[]));
        assert!(!shown_in(&fields(&[("OnlyShowIn", "KDE;")]), &[]));
    }

    #[test]
    fn several_session_desktops_all_count() {
        // `XDG_CURRENT_DESKTOP` is a list, and an entry naming any one of its
        // names belongs to this session.
        let ours = ["LineXinBar".to_string(), "KDE".to_string()];
        assert!(shown_in(&fields(&[("OnlyShowIn", "KDE;")]), &ours));
        assert!(!shown_in(&fields(&[("NotShowIn", "KDE;")]), &ours));
    }

    #[test]
    fn the_shell_settings_column_is_always_first_and_always_there() {
        let empty = assemble(Vec::new());
        assert_eq!(empty.len(), 1, "nothing installed leaves only the shell's");
        assert_eq!(empty[0].id, "settings");
        assert_eq!(empty[0].title, "Settings");
        // Rows of its own, none of which is an application: the column is the
        // shell's own controls rather than anything found on disk.
        assert!(!empty[0].entries.is_empty());
        assert_eq!(empty[0].apps(), 0);

        // Nothing found on disk lands in it, and it keeps its place ahead of
        // everything that was.
        let app =
            parse("[Desktop Entry]\nType=Application\nName=X\nExec=x\nCategories=Settings;\n")
                .unwrap();
        let categories = assemble(vec![app]);
        assert_eq!(
            categories.iter().map(|c| c.id).collect::<Vec<_>>(),
            ["settings", "system"]
        );
        assert_eq!(categories[0].apps(), 0);
        assert_eq!(categories[1].apps(), 1);
    }

    /// An application inside a subcategory is still an application in that
    /// column: everything that counts or catalogues one has to walk the tree
    /// rather than read the top of it.
    #[test]
    fn a_column_counts_what_its_subcategories_hold() {
        let buried = Category {
            id: "games",
            title: "Games",
            icon: "applications-games",
            entries: vec![Entry::Folder(Folder {
                title: "Emulators".into(),
                comment: None,
                icon: None,
                entries: vec![Entry::App(
                    parse("[Desktop Entry]\nType=Application\nName=X\nExec=x\n").unwrap(),
                )],
            })],
        };
        assert!(buried.has_app());
        assert_eq!(buried.apps(), 1);

        let hollow = Category {
            entries: vec![Entry::Folder(Folder {
                title: "Emulators".into(),
                comment: None,
                icon: None,
                entries: Vec::new(),
            })],
            ..buried.clone()
        };
        assert!(!hollow.has_app(), "a subcategory is not an application");
    }

    #[test]
    fn an_empty_column_says_which_kind_of_empty_it_is() {
        let categories = assemble(Vec::new());
        assert_eq!(
            categories[0].empty_note(),
            "LineXinBar's own settings will live here"
        );

        let scanned = Category {
            id: "games",
            title: "Games",
            icon: "applications-games",
            entries: Vec::new(),
        };
        assert_eq!(scanned.empty_note(), "No applications in this category");
    }

    #[test]
    fn ignores_keys_outside_the_main_group() {
        let app = parse(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Real\n\
             Exec=real\n\
             [Desktop Action new]\n\
             Name=Action\n\
             Exec=other\n",
        )
        .unwrap();
        assert_eq!(app.name, "Real");
        assert_eq!(app.exec, "real");
    }

    #[test]
    fn strips_field_codes() {
        assert_eq!(strip_field_codes("prog %U"), "prog");
        assert_eq!(strip_field_codes("prog %f --flag"), "prog --flag");
        assert_eq!(strip_field_codes("prog 100%% done"), "prog 100% done");
        assert_eq!(strip_field_codes("prog -i %i -c %c"), "prog -i -c");
    }

    #[test]
    fn settings_and_system_share_a_column() {
        // As in Plasma, whose menu has no Settings menu of its own. The bar's
        // Settings column belongs to the shell, not to installed software.
        for raw in ["System;Settings", "Settings", "System"] {
            let app = parse(&format!(
                "[Desktop Entry]\nType=Application\nName=X\nExec=x\nCategories={raw};\n"
            ))
            .unwrap();
            assert_eq!(app.category_id(), "system", "for {raw}");
        }
    }

    #[test]
    fn unclassifiable_entries_fall_through_to_other() {
        // Unknown categories fall through to Other.
        let app = parse("[Desktop Entry]\nType=Application\nName=X\nExec=x\nCategories=Weird;\n")
            .unwrap();
        assert_eq!(app.category_id(), "other");

        // No categories at all also lands in Other.
        let app = parse("[Desktop Entry]\nType=Application\nName=X\nExec=x\n").unwrap();
        assert_eq!(app.category_id(), "other");
    }

    #[test]
    fn audio_and_video_fold_into_multimedia() {
        for raw in ["AudioVideo", "Audio", "Video"] {
            let app = parse(&format!(
                "[Desktop Entry]\nType=Application\nName=X\nExec=x\nCategories={raw};\n"
            ))
            .unwrap();
            assert_eq!(app.category_id(), "multimedia", "for {raw}");
        }
    }
}
