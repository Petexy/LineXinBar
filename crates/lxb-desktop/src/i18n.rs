//! Shell messages. Catalogs are embedded and parsed once; identifiers and user
//! data never depend on the selected language. Static translations live with
//! the catalogs, so changing languages cannot invalidate a borrowed label.
use std::collections::BTreeMap;
#[cfg(not(test))]
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::LazyLock;

use fluent_bundle::{concurrent::FluentBundle, FluentArgs, FluentResource};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    English = 0,
    Polish = 1,
    System = 2,
}

impl Language {
    pub const ALL: [Self; 3] = [Self::System, Self::English, Self::Polish];

    pub fn key(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Polish => "pl",
            Self::System => "system",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "en" => Some(Self::English),
            "pl" => Some(Self::Polish),
            "system" => Some(Self::System),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::Polish => "Polski",
            Self::System => text("language-system"),
        }
    }
}

#[cfg(not(test))]
static PREFERENCE: AtomicU8 = AtomicU8::new(Language::English as u8);
static SYSTEM: LazyLock<String> = LazyLock::new(|| system_locale(|name| std::env::var(name).ok()));

/// POSIX precedence. An explicit C/POSIX locale stops the search, too.
fn system_locale(mut get: impl FnMut(&str) -> Option<String>) -> String {
    for name in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(value) = get(name).filter(|value| !value.trim().is_empty()) {
            return normalize(&value);
        }
    }
    "en".into()
}

fn normalize(locale: &str) -> String {
    let (base, modifier) = locale
        .trim()
        .split_once('@')
        .map_or((locale.trim(), None), |(base, modifier)| {
            (base, Some(modifier))
        });
    let base = base.split('.').next().unwrap_or("en");
    if matches!(base, "C" | "POSIX") {
        return "en".into();
    }
    let mut result = base.replace('_', "-");
    if let Some(modifier) = modifier {
        result.push('@');
        result.push_str(modifier);
    }
    result
}

#[cfg(test)]
thread_local! {
    // Localization tests cannot change the language of parallel model tests.
    static TEST_LANGUAGE: std::cell::Cell<Language> = const { std::cell::Cell::new(Language::English) };
}

pub fn preference() -> Language {
    #[cfg(test)]
    {
        TEST_LANGUAGE.with(|language| language.get())
    }
    #[cfg(not(test))]
    match PREFERENCE.load(Ordering::Relaxed) {
        1 => Language::Polish,
        2 => Language::System,
        _ => Language::English,
    }
}

pub fn set(language: Language) {
    #[cfg(test)]
    TEST_LANGUAGE.with(|current| current.set(language));
    #[cfg(not(test))]
    PREFERENCE.store(language as u8, Ordering::Relaxed);
}

pub fn locale() -> &'static str {
    match preference() {
        Language::English => "en",
        Language::Polish => "pl",
        Language::System => &SYSTEM,
    }
}

fn supported(locale: &str) -> Language {
    if locale
        .split(['-', '@'])
        .next()
        .is_some_and(|language| language.eq_ignore_ascii_case("pl"))
    {
        Language::Polish
    } else {
        Language::English
    }
}

/// XDG Desktop Entry locale candidates, most specific first. No environment
/// variables are changed: this preference belongs only to the shell.
pub fn desktop_locales() -> Vec<String> {
    let resolved = locale();
    let resolved = if supported(resolved) == Language::English && !resolved.starts_with("en") {
        "en"
    } else {
        resolved
    };
    let normalized = resolved.replace('-', "_");
    let (base, modifier) = normalized
        .split_once('@')
        .map_or((normalized.as_str(), None), |(base, modifier)| {
            (base, Some(modifier))
        });
    let language = base.split('_').next().unwrap_or(base);
    let mut candidates = vec![normalized.clone()];
    if modifier.is_some() {
        candidates.push(base.to_string());
    }
    if let Some(modifier) = modifier {
        candidates.push(format!("{language}@{modifier}"));
    }
    candidates.push(language.to_string());
    candidates.dedup();
    candidates
}

struct Catalog {
    bundle: FluentBundle<FluentResource>,
    plain: BTreeMap<String, String>,
}

impl Catalog {
    fn new(locale: &str, source: &str) -> Self {
        let resource =
            FluentResource::try_new(source.to_owned()).expect("validated embedded catalog");
        let ids: Vec<String> = source
            .lines()
            .filter_map(|line| {
                if line.starts_with(char::is_whitespace) || line.starts_with('#') {
                    return None;
                }
                line.split_once(" =").map(|(id, _)| id.to_owned())
            })
            .collect();
        let mut bundle =
            FluentBundle::new_concurrent(vec![locale.parse().expect("catalog locale")]);
        bundle.set_use_isolating(false); // Both shipped languages use left-to-right text.
        bundle
            .add_resource(resource)
            .expect("unique message identifiers");
        let plain = ids
            .into_iter()
            .filter_map(|id| {
                let pattern = bundle.get_message(&id)?.value()?;
                let mut errors = Vec::new();
                let value = bundle
                    .format_pattern(pattern, None, &mut errors)
                    .into_owned();
                errors.is_empty().then_some((id, value))
            })
            .collect();
        Self { bundle, plain }
    }

    fn format(&self, id: &str, args: &FluentArgs<'_>) -> Option<String> {
        let pattern = self.bundle.get_message(id)?.value()?;
        let mut errors = Vec::new();
        let value = self.bundle.format_pattern(pattern, Some(args), &mut errors);
        errors.is_empty().then(|| value.into_owned())
    }
}

const EN: &str = include_str!("../locales/en.ftl");
const PL: &str = include_str!("../locales/pl.ftl");
static ENGLISH: LazyLock<Catalog> = LazyLock::new(|| Catalog::new("en", EN));
static POLISH: LazyLock<Catalog> = LazyLock::new(|| Catalog::new("pl", PL));

fn catalog() -> &'static Catalog {
    match supported(locale()) {
        Language::Polish => &POLISH,
        _ => &ENGLISH,
    }
}

/// Recover the key of a borrowed catalog value, never by matching user text.
/// Constructors use this to keep model identities independent of display text.
pub fn message_id(value: &str) -> Option<&'static str> {
    static POINTERS: LazyLock<BTreeMap<(usize, usize), &'static str>> = LazyLock::new(|| {
        [&*ENGLISH, &*POLISH]
            .into_iter()
            .flat_map(|catalog| {
                catalog
                    .plain
                    .iter()
                    .map(|(key, value)| ((value.as_ptr() as usize, value.len()), key.as_str()))
            })
            .collect()
    });
    POINTERS
        .get(&(value.as_ptr() as usize, value.len()))
        .copied()
}

pub fn text(id: &'static str) -> &'static str {
    catalog()
        .plain
        .get(id)
        .or_else(|| ENGLISH.plain.get(id))
        .map(String::as_str)
        .unwrap_or(id)
}

pub fn format(id: &str, args: &FluentArgs<'_>) -> String {
    catalog()
        .format(id, args)
        .or_else(|| ENGLISH.format(id, args))
        .unwrap_or_else(|| {
            tracing::warn!(message = id, "message could not be formatted");
            id.to_owned()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogs_have_the_same_messages_and_valid_static_values() {
        let ids = |source: &str| {
            source
                .lines()
                .filter_map(|line| {
                    if line.starts_with(char::is_whitespace) || line.starts_with('#') {
                        return None;
                    }
                    line.split_once(" =").map(|(id, _)| id.to_owned())
                })
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(ids(EN), ids(PL));
        assert_eq!(
            ENGLISH.plain.keys().collect::<Vec<_>>(),
            POLISH.plain.keys().collect::<Vec<_>>()
        );
    }

    fn messages(source: &str) -> BTreeMap<String, String> {
        let mut result = BTreeMap::new();
        let mut current = String::new();
        for line in source.lines() {
            if !line.starts_with(char::is_whitespace) {
                if let Some((id, value)) = line.split_once(" = ") {
                    current = id.to_owned();
                    assert!(
                        result.insert(current.clone(), value.to_owned()).is_none(),
                        "duplicate {id}"
                    );
                    continue;
                }
            }
            if let Some(value) = result.get_mut(&current) {
                value.push_str(line);
            }
        }
        result
    }

    fn variables(message: &str) -> std::collections::BTreeSet<String> {
        message
            .split('$')
            .skip(1)
            .map(|tail| {
                tail.chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect()
            })
            .collect()
    }

    #[test]
    fn every_message_formats_and_translations_keep_the_same_arguments() {
        let en = messages(EN);
        let pl = messages(PL);
        for (id, message) in &en {
            let vars = variables(message);
            assert_eq!(vars, variables(&pl[id]), "arguments for {id}");
            let mut args = FluentArgs::new();
            for var in &vars {
                args.set(var.as_str(), 2);
            }
            assert!(ENGLISH.format(id, &args).is_some(), "English {id}");
            assert!(POLISH.format(id, &args).is_some(), "Polish {id}");
        }
    }

    #[test]
    fn source_message_references_exist_in_the_english_catalog() {
        let catalog = messages(EN);
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for file in std::fs::read_dir(root).unwrap() {
            let path = file.unwrap().path();
            if path.file_name().is_some_and(|name| name == "i18n.rs") {
                continue;
            }
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            for marker in ["i18n::text(", "crate::message!("] {
                for tail in source.split(marker).skip(1) {
                    let Some(tail) = tail.trim_start().strip_prefix('"') else {
                        continue;
                    };
                    let id = tail.split('"').next().unwrap();
                    assert!(catalog.contains_key(id), "{}: missing {id}", path.display());
                }
            }
        }
    }

    #[test]
    fn polish_counts_use_one_few_and_many_including_teens() {
        set(Language::Polish);
        for (count, expected) in [
            (0, "0 plików"),
            (1, "1 plik"),
            (2, "2 pliki"),
            (5, "5 plików"),
            (12, "12 plików"),
            (22, "22 pliki"),
            (112, "112 plików"),
        ] {
            assert_eq!(crate::message!("count-files", "count" => count), expected);
        }
        // Whole sentences with a noun the code names by a fixed word, declined
        // here with the count, and a verb agreeing with it.
        assert_eq!(
            crate::message!("search-matched", "matched" => 1, "found" => 3, "what" => "audio"),
            "Pasuje 1 z 3 plików dźwiękowych"
        );
        assert_eq!(
            crate::message!("search-matched", "matched" => 2, "found" => 1, "what" => "game"),
            "Pasują 2 z 1 gry"
        );
        assert_eq!(
            crate::message!("media-found-in-home", "count" => 22, "what" => "image"),
            "22 obrazy w katalogu domowym"
        );
        assert_eq!(
            crate::message!("count-selected", "count" => 5),
            "Zaznaczono 5 elementów"
        );
        assert_eq!(
            crate::message!("count-files-and-folders", "files" => 3, "folders" => 1),
            "3 pliki i 1 folder"
        );
        assert_eq!(
            crate::message!("time-minutes-ago", "count" => 2),
            "2 minuty temu"
        );
        assert_eq!(
            crate::message!("time-hours-ago", "count" => 5),
            "5 godzin temu"
        );
        assert_eq!(
            crate::message!("clash-many", "count" => 2, "total" => 5, "kind" => "move"),
            "2 z 5 przenoszonych elementów już znajduje się w"
        );
        assert_eq!(
            crate::message!("trash-items-still-there", "count" => 1),
            "1 element nadal jest w koszu"
        );
        set(Language::English);
        assert_eq!(
            crate::message!("search-matched", "matched" => 1, "found" => 1, "what" => "game"),
            "1 of 1 game matches"
        );
        assert_eq!(
            crate::message!("count-files-and-folders", "files" => 1, "folders" => 2),
            "1 file and 2 folders"
        );
        set(Language::Polish);
        assert_eq!(date(5, 7, 2026).unwrap(), "5 sierpnia 2026");
        assert_eq!(desktop_locales(), ["pl"]);
        // Data resembling an English label must retain its spelling.
        assert_eq!(
            crate::message!("close-target", "target" => "Settings"),
            "Zamknij Settings"
        );
        assert_eq!(message_id(&String::from("Settings")), None);
        set(Language::English);
    }

    #[test]
    fn locale_precedence_and_regional_fallback() {
        let get = |name: &str| match name {
            "LC_ALL" => Some("C.UTF-8".to_string()),
            "LC_MESSAGES" => Some("pl_PL.UTF-8".to_string()),
            _ => None,
        };
        assert_eq!(system_locale(get), "en");
        assert_eq!(normalize("pl_PL.UTF-8"), "pl-PL");
        assert_eq!(normalize("pl_PL.UTF-8@custom"), "pl-PL@custom");
        assert_eq!(supported("pl-PL"), Language::Polish);
        assert_eq!(supported("ja-JP"), Language::English);
        assert_eq!(Language::parse("not-a-language"), None);
    }

    #[test]
    fn switch_preserves_static_borrows_and_uses_english_fallback() {
        set(Language::English);
        let before = text("language-title");
        set(Language::Polish);
        assert_eq!(before, "Language");
        assert_eq!(text("language-title"), "Język");
        let partial = Catalog::new("pl", "other = Inne\n");
        assert_eq!(
            partial
                .plain
                .get("language-title")
                .or_else(|| ENGLISH.plain.get("language-title"))
                .unwrap(),
            "Language"
        );
        set(Language::English);
    }
}

/// Localize a label from a fixed, shell-owned metadata table. Never pass a
/// filename, device name, application title, or other user-supplied value here.
pub fn builtin(source: &'static str) -> &'static str {
    static KEYS: LazyLock<BTreeMap<&'static str, &'static str>> = LazyLock::new(|| {
        ENGLISH
            .plain
            .iter()
            .map(|(key, value)| (value.as_str(), key.as_str()))
            .collect()
    });
    KEYS.get(source).map_or(source, |key| text(key))
}

pub fn date(day: u32, month: usize, year: i32) -> Option<String> {
    let months = [
        "month-jan",
        "month-feb",
        "month-mar",
        "month-apr",
        "month-may",
        "month-jun",
        "month-jul",
        "month-aug",
        "month-sep",
        "month-oct",
        "month-nov",
        "month-dec",
    ];
    let mut args = FluentArgs::new();
    args.set("day", day.to_string());
    args.set("month", text(months.get(month)?));
    args.set("year", year.to_string());
    Some(format("date-full", &args))
}

/// Named arguments keep sentence order and plural selection in the catalog.
#[macro_export]
macro_rules! message {
    ($id:literal $(, $name:literal => $value:expr)* $(,)?) => {{
        let mut arguments = fluent_bundle::FluentArgs::new();
        $(arguments.set($name, $value);)*
        $crate::i18n::format($id, &arguments)
    }};
}

/// The form a shell-owned name is ordered by, for a list somebody reads down.
pub type SortKey = (Vec<u32>, Vec<u8>);

/// The letters the two shipped languages spell with, in the order a Polish
/// dictionary keeps them: each marked Polish letter is a letter of its own,
/// standing after its plain one, so "Łotwa" follows "Luksemburg" and precedes
/// "Malta". An accented letter from elsewhere — "Åland", "Réunion" — is not a
/// letter of either alphabet and files under its plain one, the mark counting
/// only against an otherwise identical name. Byte order would put every one of
/// them after Z, which is where nobody looks.
const ALPHABET: &str = "aąbcćdeęfghijklłmnńoópqrsśtuvwxyzźż";

/// Where `name` stands in a list, case folded, letters before their marks.
pub fn sort_key(name: &str) -> SortKey {
    let mut primary = Vec::with_capacity(name.len());
    let mut secondary = Vec::with_capacity(name.len());
    for c in name.chars().flat_map(char::to_lowercase) {
        let (base, mark) = match c {
            'å' | 'á' | 'à' | 'â' | 'ä' | 'ã' => ('a', 1),
            'é' | 'è' | 'ê' | 'ë' => ('e', 1),
            'í' | 'ì' | 'î' | 'ï' => ('i', 1),
            'ô' | 'ò' | 'ö' | 'õ' | 'ø' => ('o', 1),
            'ú' | 'ù' | 'û' | 'ü' => ('u', 1),
            'ç' => ('c', 1),
            'ñ' => ('n', 1),
            'ý' | 'ÿ' => ('y', 1),
            other => (other, 0),
        };
        // Space and punctuation before every letter, so "Guinea" precedes
        // "Guinea-Bissau" and "Saint Lucia" precedes "Saintfield"; digits and
        // anything else after, by code point.
        let rank = match ALPHABET.chars().position(|letter| letter == base) {
            Some(at) => 1 + at as u32,
            None if base.is_whitespace() || base.is_ascii_punctuation() => 0,
            None => 100 + base as u32,
        };
        primary.push(rank);
        secondary.push(mark);
    }
    (primary, secondary)
}

/// Localize the decimal separator in a shell-formatted numeric quantity.
/// Never pass arbitrary user text, filenames, or protocol values here.
pub fn decimal(number: String) -> String {
    if supported(locale()) == Language::Polish {
        number.replace('.', ",")
    } else {
        number
    }
}
