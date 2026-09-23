//! Shell messages. Catalogs are embedded and parsed once; identifiers and user
//! data never depend on the selected language. Static translations live with
//! the catalogs, so changing languages cannot invalidate a borrowed label.
use std::collections::BTreeMap;
#[cfg(not(test))]
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::LazyLock;

use fluent_bundle::{concurrent::FluentBundle, FluentArgs, FluentResource, FluentValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    /// English as the shell is written in it, and the language every other
    /// one falls back to a message from. `en-GB.ftl` is the whole catalog;
    /// see [`American`](Self::American), which is not.
    British = 0,
    Polish = 1,
    /// Nobody has chosen one: the shell follows the session, and touches
    /// nothing. Not a choice a row offers — see [`Language::CHOICES`].
    System = 2,
    /// English as America writes it: the same language with the month at the
    /// front of a date and a handful of words spelled differently, so its
    /// catalog is an overlay of a few dozen messages over the British one
    /// rather than a copy of it. See [`AMERICAN`] and `locales/en-US.ftl`.
    ///
    /// Last, and numbered last, because the three above it were the whole
    /// enum before the split and their numbers are what [`PREFERENCE`] holds.
    American = 3,
    French = 4,
    Spanish = 5,
    German = 6,
    Hindi = 7,
    /// Brazilian Portuguese, and the Portuguese a session in `pt_PT` is read
    /// in as well: a reader of the other variant is better answered by it
    /// than by English. See [`supported`].
    Portuguese = 8,
    Russian = 9,
    /// Simplified Chinese, and likewise what `zh_TW` and `zh_HK` are read in.
    Chinese = 10,
}

impl Language {
    /// The languages a person can choose: every one the shell has a catalog
    /// for. Choosing one sets the *system* language ([`crate::locale`]), which
    /// is why [`Language::System`] is not among them: a list whose entries set
    /// the system has no room for an entry meaning "whatever the system has".
    ///
    /// The order is the order the languages' **own names** sort in, which is
    /// the order somebody looking for theirs reads down: the Latin names
    /// first, Deutsch to Português, then Русский, then हिन्दी, then 简体中文 —
    /// each alphabet after the one before it, as a list of names in several
    /// alphabets is ordered everywhere. Not the order of this enum, and not
    /// English's alphabet applied to English's names for them.
    pub const CHOICES: [Self; 10] = [
        Self::German,
        Self::British,
        Self::American,
        Self::Spanish,
        Self::French,
        Self::Polish,
        Self::Portuguese,
        Self::Russian,
        Self::Hindi,
        Self::Chinese,
    ];

    /// What `shell.toml` writes, and the tag a `.desktop` file's `Name[xx]`
    /// and the system locale are built from.
    pub fn key(self) -> &'static str {
        match self {
            Self::British => "en-GB",
            Self::American => "en-US",
            Self::French => "fr",
            Self::Spanish => "es",
            Self::Polish => "pl",
            Self::German => "de",
            Self::Hindi => "hi",
            Self::Portuguese => "pt-BR",
            Self::Russian => "ru",
            Self::Chinese => "zh-CN",
            Self::System => "system",
        }
    }

    /// Read back what [`Language::key`] wrote.
    ///
    /// A bare `en` is what every file written before the two Englishes were
    /// told apart says, and it is read as British: that is the catalog those
    /// shells drew their words from, so a file from one of them goes on saying
    /// what it always said. Choosing either row rewrites it to a tag that
    /// names a region.
    pub fn parse(value: &str) -> Option<Self> {
        match value.replace('_', "-").as_str() {
            "en-GB" | "en" => Some(Self::British),
            "en-US" => Some(Self::American),
            "fr" => Some(Self::French),
            "es" => Some(Self::Spanish),
            "pl" => Some(Self::Polish),
            "de" => Some(Self::German),
            "hi" => Some(Self::Hindi),
            "pt-BR" => Some(Self::Portuguese),
            "ru" => Some(Self::Russian),
            "zh-CN" => Some(Self::Chinese),
            "system" => Some(Self::System),
            _ => None,
        }
    }

    /// Steam's own name for the language — what its web services take as
    /// `l=` and its agreements as `eulaLang=`.
    ///
    /// Steam has no Hindi, so a Hindi shell is answered in English, which is
    /// what Steam itself falls back to; the two Englishes are one language to
    /// it. Spanish is Spain's, which is the catalog this shell writes.
    pub fn steam_name(self) -> &'static str {
        match self {
            Self::British | Self::American | Self::Hindi => "english",
            Self::French => "french",
            Self::Spanish => "spanish",
            Self::Polish => "polish",
            Self::German => "german",
            Self::Portuguese => "brazilian",
            Self::Russian => "russian",
            Self::Chinese => "schinese",
            Self::System => spoken().steam_name(),
        }
    }

    /// The same tag in the form gettext's `LANGUAGE` list is written in.
    ///
    /// That list holds *locale* names, and glibc splits one into a language
    /// and a territory at an underscore. `en-GB` with a hyphen is one long
    /// language name it has no catalog for and would fall through to nothing,
    /// where `en_GB` finds the British catalog and then the plain English one
    /// behind it. See [`crate::locale`], which writes it.
    pub fn gettext_name(self) -> &'static str {
        match self {
            Self::British => "en_GB",
            Self::American => "en_US",
            Self::French => "fr",
            Self::Spanish => "es",
            Self::Polish => "pl",
            Self::German => "de",
            Self::Hindi => "hi",
            Self::Portuguese => "pt_BR",
            Self::Russian => "ru",
            Self::Chinese => "zh_CN",
            Self::System => "system",
        }
    }

    /// The language's own name for itself, which is the same in every
    /// language the shell speaks.
    ///
    /// The two Englishes are told apart by the country rather than by the
    /// word: "English (UK)" and "English (US)" are what a console offers and
    /// what somebody looking for their own date format will recognise, and
    /// neither is a translation of the other. Portuguese names its country
    /// for the same reason, and Chinese names its script: each is the one
    /// variant the shell writes, and a reader of the other should be able to
    /// see that before pressing it.
    ///
    /// Two of these are in alphabets Roboto has not got, which is why the
    /// Devanagari and Han faces travel with the shell whatever language it is
    /// in — see `gpu::UI_FONT_FALLBACKS`. A list of languages that could not
    /// write two of its own rows would be no list at all.
    pub fn name(self) -> &'static str {
        match self {
            Self::British => "English (UK)",
            Self::American => "English (US)",
            Self::French => "Français",
            Self::Spanish => "Español",
            Self::Polish => "Polski",
            Self::German => "Deutsch",
            Self::Hindi => "हिन्दी",
            Self::Portuguese => "Português (Brasil)",
            Self::Russian => "Русский",
            Self::Chinese => "简体中文",
            Self::System => spoken().name(),
        }
    }
}

/// Which of the two clocks a time of day is written on.
///
/// Its own setting rather than a fact about the language, because the two
/// questions really are separate: somebody who reads English as America
/// writes it may still want the twenty-four hour clock a console shows, and
/// somebody in Warsaw may want the twelve-hour one. Settings > System > Clock
/// is the row, and it is on that page rather than under Language because what
/// it changes is how this machine writes, not what language it writes in.
///
/// One answer for the whole session and for everything the session opens: it
/// is written to `shell.toml` as `clock`, which is where an application built
/// on lxb-toolkit and the login screen both ask the same question. See
/// [`crate::settings::clock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clock {
    /// Nobody has chosen, so the language answers: American English and
    /// Hindi write half past eight in the evening as `8:30 PM`, which is the
    /// clock India and America both read, and every other language the
    /// shell speaks writes it as `20:30`.
    ///
    /// The default, and what every file written before this setting existed
    /// says by saying nothing — so a shell whose language is not English (US)
    /// goes on writing exactly the times it always wrote.
    FromLanguage = 0,
    TwentyFourHour = 1,
    TwelveHour = 2,
}

impl Clock {
    /// The two a row offers. [`Clock::FromLanguage`] is not among them for
    /// the reason [`Language::System`] is not among that list's: the rows are
    /// the two clocks there are, and "whichever my language uses" is what the
    /// page says before either has been pressed rather than a third clock.
    pub const CHOICES: [Self; 2] = [Self::TwentyFourHour, Self::TwelveHour];

    pub fn key(self) -> &'static str {
        match self {
            Self::FromLanguage => "language",
            Self::TwentyFourHour => "24-hour",
            Self::TwelveHour => "12-hour",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "language" => Some(Self::FromLanguage),
            "24-hour" => Some(Self::TwentyFourHour),
            "12-hour" => Some(Self::TwelveHour),
            _ => None,
        }
    }
}

#[cfg(not(test))]
static CLOCK: AtomicU8 = AtomicU8::new(Clock::FromLanguage as u8);

#[cfg(test)]
thread_local! {
    // As with the language: a test about the clock cannot be allowed to move
    // the clock under a test running beside it.
    static TEST_CLOCK: std::cell::Cell<Clock> = const { std::cell::Cell::new(Clock::FromLanguage) };
}

/// What the row has been set to, which is not the same question as
/// [`twelve_hour`]: a page marks the row somebody chose, and nothing is
/// marked until somebody has.
pub fn clock() -> Clock {
    #[cfg(test)]
    {
        TEST_CLOCK.with(|clock| clock.get())
    }
    #[cfg(not(test))]
    match CLOCK.load(Ordering::Relaxed) {
        1 => Clock::TwentyFourHour,
        2 => Clock::TwelveHour,
        _ => Clock::FromLanguage,
    }
}

pub fn set_clock(chosen: Clock) {
    #[cfg(test)]
    TEST_CLOCK.with(|clock| clock.set(chosen));
    #[cfg(not(test))]
    CLOCK.store(chosen as u8, Ordering::Relaxed);
}

/// Whether a time is written with AM or PM after it. The one question
/// [`time_of_day`] asks, and the one the login screen and the toolkit ask of
/// `shell.toml`.
pub fn twelve_hour() -> bool {
    twelve_hour_on(clock())
}

/// The same question of a clock named rather than the one in force, for the
/// page that offers both and for the row that has to mark the one in force
/// when nobody has chosen either.
pub fn twelve_hour_on(clock: Clock) -> bool {
    match clock {
        Clock::TwelveHour => true,
        Clock::TwentyFourHour => false,
        Clock::FromLanguage => matches!(spoken(), Language::American | Language::Hindi),
    }
}

#[cfg(not(test))]
static PREFERENCE: AtomicU8 = AtomicU8::new(Language::British as u8);
/// What the session spoke when the shell started, which is what a shell with
/// no language chosen follows. Read once: [`crate::locale`] rewrites the
/// environment when a language *is* chosen, and that is not this.
static SYSTEM: LazyLock<String> = LazyLock::new(session_locale);

/// What the session speaks now, read afresh from the environment.
pub fn session_locale() -> String {
    system_locale(|name| std::env::var(name).ok())
}

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
    static TEST_LANGUAGE: std::cell::Cell<Language> = const { std::cell::Cell::new(Language::British) };
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
        3 => Language::American,
        4 => Language::French,
        5 => Language::Spanish,
        6 => Language::German,
        7 => Language::Hindi,
        8 => Language::Portuguese,
        9 => Language::Russian,
        10 => Language::Chinese,
        _ => Language::British,
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
        Language::System => &SYSTEM,
        chosen => chosen.key(),
    }
}

/// Which catalog a locale name is read out of.
///
/// The region is looked at for one language and one reason: `en_US` is the
/// only English the shell writes differently, so it is the only one named.
/// Every other English there is — `en_GB`, `en_AU`, `en_IE`, a bare `en` — is
/// the British catalog, which is the English the shell is written in. A
/// language with no catalog at all is English too, and that is the fallback
/// every untranslated message in the session already takes.
///
/// Two catalogs carry a region in their name without one being looked at:
/// `pt-BR` and `zh-CN` are the Portuguese and the Chinese that were written,
/// so `pt_PT` and `zh_TW` are read in them. A reader of European Portuguese
/// or of traditional characters is answered better by the other variant of
/// their own language than by English, which is what the alternative is.
fn supported(locale: &str) -> Language {
    let mut parts = locale.split(['-', '@']);
    let base = parts.next().unwrap_or_default();
    let region = parts.next().unwrap_or_default();
    if base.eq_ignore_ascii_case("pl") {
        Language::Polish
    } else if base.eq_ignore_ascii_case("fr") {
        Language::French
    } else if base.eq_ignore_ascii_case("es") {
        Language::Spanish
    } else if base.eq_ignore_ascii_case("de") {
        Language::German
    } else if base.eq_ignore_ascii_case("hi") {
        Language::Hindi
    } else if base.eq_ignore_ascii_case("pt") {
        Language::Portuguese
    } else if base.eq_ignore_ascii_case("ru") {
        Language::Russian
    } else if base.eq_ignore_ascii_case("zh") {
        Language::Chinese
    } else if base.eq_ignore_ascii_case("en") && region.eq_ignore_ascii_case("US") {
        Language::American
    } else {
        Language::British
    }
}

/// The language the catalogs are really drawn from: the preference, or for a
/// shell nobody has chosen one on, whichever the session speaks. Never
/// [`Language::System`].
pub fn spoken() -> Language {
    supported(locale())
}

/// The language a locale name would be read in — `pl_PL.UTF-8`, `pl-PL` and
/// `pl` are Polish, and everything the shell has no catalog for is English.
pub fn spoken_by(locale: &str) -> Language {
    supported(&normalize(locale))
}

/// XDG Desktop Entry locale candidates, most specific first, for the language
/// the shell speaks. Read by the scan of `.desktop` files, which happens
/// before and apart from anything [`crate::locale`] does to the environment.
pub fn desktop_locales() -> Vec<String> {
    let resolved = locale();
    // A session in a language the shell has no catalog for is drawn in
    // English, so the names it looks for in a `.desktop` file are English
    // ones — `Name[en_GB]`, then `Name[en]`.
    let resolved = if ["en", "pl", "fr", "es", "de", "hi", "pt", "ru", "zh"]
        .iter()
        .any(|base| resolved.starts_with(base))
    {
        resolved
    } else {
        spoken().key()
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
        bundle.set_use_isolating(false); // Every shipped language is written left to right.
                                         // `PAD2($n)`: a day or a month written with two digits. Whether a date
                                         // is `9/16` or `16.09` is the language's decision, so it is made in the
                                         // catalog; fluent-bundle parses NUMBER's minimumIntegerDigits and
                                         // then does not apply it, which is why this is a function of its own.
        bundle
            .add_function("PAD2", |positional, _named| match positional.first() {
                Some(FluentValue::Number(number)) => {
                    FluentValue::String(format!("{:02}", number.value as i64).into())
                }
                Some(FluentValue::String(text)) => FluentValue::String(
                    text.parse::<i64>()
                        .map_or_else(|_| text.clone(), |n| format!("{n:02}").into()),
                ),
                _ => FluentValue::Error,
            })
            .expect("PAD2 is registered once");
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

const EN_GB: &str = include_str!("../locales/en-GB.ftl");
const EN_US: &str = include_str!("../locales/en-US.ftl");
const FR: &str = include_str!("../locales/fr.ftl");
const ES: &str = include_str!("../locales/es.ftl");
const PL: &str = include_str!("../locales/pl.ftl");
const DE: &str = include_str!("../locales/de.ftl");
const HI: &str = include_str!("../locales/hi.ftl");
const PT_BR: &str = include_str!("../locales/pt-BR.ftl");
const RU: &str = include_str!("../locales/ru.ftl");
const ZH_CN: &str = include_str!("../locales/zh-CN.ftl");
/// Every message there is. Each of the other two is read first and this one
/// answers whatever they have not got, which is what lets `en-US.ftl` be a
/// page of differences rather than a second copy of the shell's whole text.
static BRITISH: LazyLock<Catalog> = LazyLock::new(|| Catalog::new("en-GB", EN_GB));
static AMERICAN: LazyLock<Catalog> = LazyLock::new(|| Catalog::new("en-US", EN_US));
static FRENCH: LazyLock<Catalog> = LazyLock::new(|| Catalog::new("fr", FR));
static SPANISH: LazyLock<Catalog> = LazyLock::new(|| Catalog::new("es", ES));
static POLISH: LazyLock<Catalog> = LazyLock::new(|| Catalog::new("pl", PL));
static GERMAN: LazyLock<Catalog> = LazyLock::new(|| Catalog::new("de", DE));
static HINDI: LazyLock<Catalog> = LazyLock::new(|| Catalog::new("hi", HI));
static PORTUGUESE: LazyLock<Catalog> = LazyLock::new(|| Catalog::new("pt-BR", PT_BR));
static RUSSIAN: LazyLock<Catalog> = LazyLock::new(|| Catalog::new("ru", RU));
static CHINESE: LazyLock<Catalog> = LazyLock::new(|| Catalog::new("zh-CN", ZH_CN));

/// The catalog read first. [`BRITISH`] is read after it, by everything that
/// reads this — an overlay is the ordinary case here, not the exception.
fn catalog() -> &'static Catalog {
    match spoken() {
        Language::Polish => &POLISH,
        Language::French => &FRENCH,
        Language::Spanish => &SPANISH,
        Language::German => &GERMAN,
        Language::Hindi => &HINDI,
        Language::Portuguese => &PORTUGUESE,
        Language::Russian => &RUSSIAN,
        Language::Chinese => &CHINESE,
        Language::American => &AMERICAN,
        // `spoken` never answers `System`, and British is the fallback.
        _ => &BRITISH,
    }
}

/// Recover the key of a borrowed catalog value, never by matching user text.
/// Constructors use this to keep model identities independent of display text.
pub fn message_id(value: &str) -> Option<&'static str> {
    static POINTERS: LazyLock<BTreeMap<(usize, usize), &'static str>> = LazyLock::new(|| {
        [
            &*BRITISH,
            &*AMERICAN,
            &*FRENCH,
            &*SPANISH,
            &*POLISH,
            &*GERMAN,
            &*HINDI,
            &*PORTUGUESE,
            &*RUSSIAN,
            &*CHINESE,
        ]
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
        .or_else(|| BRITISH.plain.get(id))
        .map(String::as_str)
        .unwrap_or(id)
}

pub fn format(id: &str, args: &FluentArgs<'_>) -> String {
    catalog()
        .format(id, args)
        .or_else(|| BRITISH.format(id, args))
        .unwrap_or_else(|| {
            tracing::warn!(message = id, "message could not be formatted");
            id.to_owned()
        })
}

/// Localize a label from a fixed, shell-owned metadata table. Never pass a
/// filename, device name, application title, or other user-supplied value here.
///
/// Both Englishes are looked in, and the British one second so it wins a tie:
/// a table in this shell's source may be written with either spelling, and
/// an "Accent color" written into one has to find the row the British catalog
/// calls "Accent colour".
pub fn builtin(source: &'static str) -> &'static str {
    static KEYS: LazyLock<BTreeMap<&'static str, &'static str>> = LazyLock::new(|| {
        [&*AMERICAN, &*BRITISH]
            .into_iter()
            .flat_map(|catalog| {
                catalog
                    .plain
                    .iter()
                    .map(|(key, value)| (value.as_str(), key.as_str()))
            })
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

/// The same date in figures, for a row with no room for a month's name:
/// `16/09/2026`, `09/16/2026` or `16.09.2026`. `month` is 1 to 12.
///
/// Every value goes in as text rather than as a number. Fluent formats a
/// number for the locale, so a year handed over as one comes back grouped —
/// `2,026` — and a date is not a quantity.
pub fn date_in_figures(day: u32, month: u32, year: i32) -> String {
    crate::message!("date-numeric",
        "day" => day.to_string(),
        "month" => month.to_string(),
        "year" => year.to_string(),
    )
}

/// The time of day, on whichever clock the session is set to: `20:10`, or
/// `8:10 PM`. `hour` is 0 to 23.
///
/// Every clock the user reads comes through here — the start screen's corner,
/// the guide's header, the night light's schedule, the trash, the updates
/// history, an achievement's unlock, the message a conversation's light is
/// standing on. A time written into a *file name* or a
/// file *format* does not: the trash's `DeletionDate` is RFC 3339 and a
/// screenshot is called what sorts, and neither is somebody's setting to
/// change. See [`crate::trash`] and [`crate::screenshot`].
pub fn time_of_day(hour: u32, minute: u32) -> String {
    time_of_day_on(clock(), hour, minute)
}

/// The same, on a clock named rather than the one in force: what Settings >
/// System > Clock writes under each of its two rows.
pub fn time_of_day_on(clock: Clock, hour: u32, minute: u32) -> String {
    let (hour, minute) = (hour.min(23), minute.min(59));
    if !twelve_hour_on(clock) {
        return crate::message!("clock-24-hour",
            "hour" => hour.to_string(),
            "minute" => minute.to_string(),
        );
    }
    // Midnight is twelve, not zero, and noon is twelve as well: the hour
    // rolls to twelve at each end rather than counting from it.
    let half = text(if hour < 12 { "clock-am" } else { "clock-pm" });
    let shown = match hour % 12 {
        0 => 12,
        other => other,
    };
    crate::message!("clock-12-hour",
        "hour" => shown.to_string(),
        "minute" => minute.to_string(),
        "half" => half,
    )
}

/// When something happened, said the two ways a note beside it might have the
/// room for.
///
/// Both are built at once rather than at the place that draws them, because
/// *which* of the two is used is a question about the room there is — see
/// `ui::friends_said_at`, which measures the longer one against the
/// space beside a message and falls back to the shorter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Moment {
    /// The time of day alone, on the clock the session is set to: `14:23`.
    pub time: String,
    /// The same moment with the day it fell on: `21/9 14:23`. `None` for
    /// today, whose date is the one thing everybody reading a screen already
    /// knows — and the case a note beside a message is nearly always in.
    pub with_the_day: Option<String>,
}

/// Break one moment down for a note beside something, against the wall clock
/// as it is now.
///
/// `seconds` is an ordinary Unix time, in the machine's own `time_t` so that
/// nothing here has to decide what to do about a stamp that will not fit in
/// one. `None` where the C library would not break it down at all.
pub fn moment(seconds: libc::time_t) -> Option<Moment> {
    moment_on(seconds, now())
}

/// The same, against a `now` that is stated rather than read — which is what
/// lets a test say what "today" is without waiting for tomorrow to break it.
pub fn moment_on(seconds: libc::time_t, now: Option<libc::time_t>) -> Option<Moment> {
    let tm = broken_down(seconds)?;
    let time = time_of_day(
        tm.tm_hour.clamp(0, 23) as u32,
        tm.tm_min.clamp(0, 59) as u32,
    );
    // The same day in the same zone, which is a year and a day *of* that year
    // rather than arithmetic on seconds: a local day is twenty-three hours
    // long once a year and twenty-five once, and in some zones it is neither.
    let today = now
        .and_then(broken_down)
        .is_some_and(|now| (now.tm_year, now.tm_yday) == (tm.tm_year, tm.tm_yday));
    let with_the_day = (!today).then(|| {
        // `clock-corner` is this shell's compact day and time: the two numbers
        // in the order the language writes them — 21/9 here, 9/21 in America,
        // 21.09. in Germany — around a time on whichever clock the session is
        // set to. Named for the start screen's corner, which is where it was
        // first needed, and read here rather than copied into a second message
        // so that a language settles that order once. See `wall_clock`.
        crate::message!("clock-corner",
            "day" => tm.tm_mday.clamp(1, 31).to_string(),
            "month" => (tm.tm_mon.clamp(0, 11) + 1).to_string(),
            "time" => time.clone(),
        )
    });
    Some(Moment { time, with_the_day })
}

/// The wall clock now, in seconds since the epoch.
fn now() -> Option<libc::time_t> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|since| since.as_secs() as libc::time_t)
}

/// One moment in the machine's own zone, through the C library rather than any
/// arithmetic of this shell's: which year is a leap year, which zone this
/// machine is in and the hour a country moves its clocks are all its answers
/// to give.
fn broken_down(seconds: libc::time_t) -> Option<libc::tm> {
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: both pointers are valid, and `tm` is read only where
    // `localtime_r` said it filled it in.
    if unsafe { libc::localtime_r(&seconds, tm.as_mut_ptr()) }.is_null() {
        return None;
    }
    Some(unsafe { tm.assume_init() })
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

/// The Latin letters the shipped languages spell with, in the order a Polish
/// dictionary keeps them: each marked Polish letter is a letter of its own,
/// standing after its plain one, so "Łotwa" follows "Luksemburg" and precedes
/// "Malta". An accented letter from elsewhere — "Åland", "Réunion", the
/// German "Österreich" — is not a letter of its own and files under its plain
/// one, the mark counting only against an otherwise identical name. Byte
/// order would put every one of them after Z, which is where nobody looks.
///
/// The other three alphabets the shell writes order themselves: Cyrillic,
/// Devanagari and the Han characters all stand in their code points in the
/// order their own dictionaries keep them, or near enough — the one Russian
/// letter that does not, ё, is a marked е below. A Chinese list sorts by
/// code point rather than by pinyin, which is the order a dictionary of
/// radicals keeps and not the one a reader expects; see [`sort_key`].
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
            // Russian files ё with е, the two dots counting only against an
            // otherwise identical name; by code point it would stand after я.
            'ё' => ('е', 1),
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
    // German, Spanish, French, Polish, Portuguese and Russian write the
    // fraction after a comma; the two Englishes, Hindi and Chinese after a
    // full stop. The separator is the language's, not the country's, so this
    // is a question about the catalog rather than about the locale the
    // system happens to be in.
    if matches!(
        spoken(),
        Language::British | Language::American | Language::Hindi | Language::Chinese
    ) {
        number
    } else {
        number.replace('.', ",")
    }
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
        // Every *whole* catalog carries every message. `en-US` is the one
        // exception and is held to the opposite rule by the test below it.
        for (name, source, catalog) in whole_catalogs() {
            assert_eq!(ids(EN_GB), ids(source), "{name} carries other messages");
            assert_eq!(
                BRITISH.plain.keys().collect::<Vec<_>>(),
                catalog.plain.keys().collect::<Vec<_>>(),
                "{name} formats a different set of plain messages"
            );
        }
    }

    /// Every language with a catalog of its own, by the name a failure should
    /// print. `en-US` is not among them: it is an overlay, not a translation.
    fn whole_catalogs() -> [(&'static str, &'static str, &'static Catalog); 9] {
        [
            ("en-GB", EN_GB, &BRITISH),
            ("fr", FR, &FRENCH),
            ("es", ES, &SPANISH),
            ("pl", PL, &POLISH),
            ("de", DE, &GERMAN),
            ("hi", HI, &HINDI),
            ("pt-BR", PT_BR, &PORTUGUESE),
            ("ru", RU, &RUSSIAN),
            ("zh-CN", ZH_CN, &CHINESE),
        ]
    }

    /// The American catalog is an overlay and has to stay one.
    ///
    /// Every message in it is a message the British catalog has, so nothing in
    /// it can be a message the shell asks for and no other language carries;
    /// and none of them says the same thing the British one does, because a
    /// line copied across unchanged is a line that has to be edited twice from
    /// the day it is copied and will one day not be.
    #[test]
    fn the_american_catalog_is_only_what_america_writes_differently() {
        let british = messages(EN_GB);
        let american = messages(EN_US);
        assert!(!american.is_empty(), "there is a difference to carry");
        assert!(
            american.len() * 20 < british.len(),
            "{} of {} messages is a second catalog, not an overlay",
            american.len(),
            british.len()
        );
        for (id, value) in &american {
            let Some(theirs) = british.get(id) else {
                panic!("en-US writes {id}, which no other catalog has");
            };
            assert_ne!(theirs, value, "en-US copies {id} out of en-GB unchanged");
            assert_eq!(
                variables(value),
                variables(theirs),
                "the American {id} takes other variables"
            );
        }
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
        let en = messages(EN_GB);
        let american = messages(EN_US);
        let translations: Vec<(&str, BTreeMap<String, String>, &Catalog)> = whole_catalogs()
            .into_iter()
            .map(|(name, source, catalog)| (name, messages(source), catalog))
            .collect();
        for (id, message) in &en {
            let vars = variables(message);
            let mut args = FluentArgs::new();
            for var in &vars {
                args.set(var.as_str(), 2);
            }
            for (name, theirs, catalog) in &translations {
                assert_eq!(
                    vars,
                    variables(&theirs[id]),
                    "the {name} translation of {id} takes other arguments"
                );
                assert!(catalog.format(id, &args).is_some(), "{name} {id}");
            }
            if american.contains_key(id) {
                assert!(AMERICAN.format(id, &args).is_some(), "American {id}");
            }
        }
    }

    #[test]
    fn source_message_references_exist_in_the_english_catalog() {
        let catalog = messages(EN_GB);
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

    /// French and Spanish have two forms, and they disagree about zero.
    ///
    /// French counts nought as singular — *0 fichier* — and Spanish does not:
    /// *0 archivos*. Neither is a rule written here; both are CLDR's, which is
    /// the whole reason a count is handed to Fluent as a **number**. Handed a
    /// string it would match nothing and every language would silently get the
    /// `other` branch, which for French would be wrong at exactly one value
    /// and therefore never noticed.
    #[test]
    fn french_and_spanish_agree_with_cldr_about_nought() {
        set(Language::French);
        for (count, expected) in [
            (0, "0 fichier"),
            (1, "1 fichier"),
            (2, "2 fichiers"),
            (22, "22 fichiers"),
        ] {
            assert_eq!(crate::message!("count-files", "count" => count), expected);
        }
        assert_eq!(
            crate::message!("media-found-in-home", "count" => 1, "what" => "image"),
            "1 image dans votre dossier personnel"
        );
        assert_eq!(
            crate::message!("search-matched", "matched" => 2, "found" => 3, "what" => "audio"),
            "2 correspondances sur 3 fichiers audio"
        );

        set(Language::Spanish);
        for (count, expected) in [
            (0, "0 archivos"),
            (1, "1 archivo"),
            (2, "2 archivos"),
            (22, "22 archivos"),
        ] {
            assert_eq!(crate::message!("count-files", "count" => count), expected);
        }
        assert_eq!(
            crate::message!("media-found-in-home", "count" => 1, "what" => "image"),
            "1 imagen en su carpeta personal"
        );
        assert_eq!(
            crate::message!("search-matched", "matched" => 2, "found" => 3, "what" => "audio"),
            "2 coincidencias de 3 archivos de audio"
        );

        // And the decimal mark: a comma in the six languages that write one,
        // a full stop in the two Englishes and in the two languages of the
        // other two scripts.
        for language in [
            Language::French,
            Language::Spanish,
            Language::Polish,
            Language::German,
            Language::Portuguese,
            Language::Russian,
        ] {
            set(language);
            assert_eq!(decimal("4.5 GB".to_string()), "4,5 GB");
        }
        for language in [
            Language::British,
            Language::American,
            Language::Hindi,
            Language::Chinese,
        ] {
            set(language);
            assert_eq!(decimal("4.5 GB".to_string()), "4.5 GB");
        }
        set(Language::British);
    }

    /// The five languages that followed, each on CLDR's own rule for it.
    ///
    /// Russian has the three forms Polish has and draws the lines in the
    /// same places, except that 22 is *few* in both and 12 is *many* in
    /// both — the teens are the whole reason the rule is not "ends in 1".
    /// Hindi and Brazilian Portuguese count nought as singular, as French
    /// does; German does not; and Chinese has one form for every number, so
    /// its catalog writes no selector at all.
    #[test]
    fn the_five_later_languages_agree_with_cldr_about_their_plurals() {
        set(Language::Russian);
        for (count, expected) in [
            (0, "0 файлов"),
            (1, "1 файл"),
            (2, "2 файла"),
            (5, "5 файлов"),
            (11, "11 файлов"),
            (12, "12 файлов"),
            (21, "21 файл"),
            (22, "22 файла"),
            (112, "112 файлов"),
        ] {
            assert_eq!(crate::message!("count-files", "count" => count), expected);
        }
        assert_eq!(
            crate::message!("search-matched", "matched" => 1, "found" => 3, "what" => "audio"),
            "Подходит 1 из 3 аудиофайлов"
        );
        assert_eq!(
            crate::message!("media-found-in-home", "count" => 22, "what" => "image"),
            "22 изображения в вашей домашней папке"
        );
        assert_eq!(
            crate::message!("time-hours-ago", "count" => 5),
            "5 часов назад"
        );

        set(Language::German);
        for (count, expected) in [(0, "0 Dateien"), (1, "1 Datei"), (2, "2 Dateien")] {
            assert_eq!(crate::message!("count-files", "count" => count), expected);
        }
        assert_eq!(
            crate::message!("search-matched", "matched" => 1, "found" => 3, "what" => "game"),
            "1 von 3 Spielen passt"
        );

        set(Language::Portuguese);
        for (count, expected) in [(0, "0 arquivo"), (1, "1 arquivo"), (2, "2 arquivos")] {
            assert_eq!(crate::message!("count-files", "count" => count), expected);
        }

        set(Language::Hindi);
        for (count, expected) in [(0, "0 फ़ाइल"), (1, "1 फ़ाइल"), (2, "2 फ़ाइलें")]
        {
            assert_eq!(crate::message!("count-files", "count" => count), expected);
        }

        set(Language::Chinese);
        for (count, expected) in [(0, "0 个文件"), (1, "1 个文件"), (22, "22 个文件")] {
            assert_eq!(crate::message!("count-files", "count" => count), expected);
        }
        assert_eq!(
            crate::message!("search-matched", "matched" => 2, "found" => 3, "what" => "audio"),
            "3 个音频文件中有 2 个匹配"
        );
        set(Language::British);
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
        // One agreement is "this one", and a count of them is declined with
        // the count.
        for (total, expected) in [
            (
                1,
                "Aby zainstalować Garry's Mod, musisz zaakceptować tę umowę.",
            ),
            (
                2,
                "Aby zainstalować Garry's Mod, musisz zaakceptować 2 umowy. To jest umowa nr 1.",
            ),
            (
                5,
                "Aby zainstalować Garry's Mod, musisz zaakceptować 5 umów. To jest umowa nr 1.",
            ),
        ] {
            assert_eq!(
                crate::message!(
                    "steam-agreement-before-install",
                    "game" => "Garry's Mod",
                    "at" => 1,
                    "total" => total
                ),
                expected
            );
        }
        // What an install waits on is a word the catalog makes a sentence of,
        // never an English phrase inside a Polish one.
        assert_eq!(
            crate::message!("steam-install-needs-first", "what" => "key"),
            "Ta gra najpierw wymaga wpisania klucza produktu."
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
        set(Language::British);
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
        set(Language::British);
    }

    /// The corner's clock is drawn from a closed set of characters cut from
    /// the face — digits, the colon, the slash, the full stop, the per cent
    /// sign, the space and the three letters AM and PM are written with — and
    /// a run with a character outside it is not drawn at all. So a language's
    /// `clock-corner`, `clock-24-hour`, `clock-12-hour`, `clock-am` and
    /// `clock-pm` may use no others: the Polish `16.09` lost the whole clock
    /// before the full stop was in the set.
    #[test]
    fn every_clock_format_is_drawn_from_the_corners_set() {
        for language in Language::CHOICES {
            set(language);
            let clock = crate::message!("clock-corner",
                "month" => "9", "day" => "6", "time" => "20:10");
            assert_eq!(
                clock,
                match language {
                    Language::Polish | Language::Russian => "06.09 20:10",
                    Language::German => "6.9. 20:10",
                    // The month goes first in America, and in China where the
                    // year would go before it if there were room for one.
                    Language::American | Language::Chinese => "9/6 20:10",
                    // The day goes first everywhere else.
                    _ => "6/9 20:10",
                }
            );
            let mut runs = vec![clock];
            for clock in Clock::CHOICES {
                set_clock(clock);
                // Every hour of the day, so both halves of the twelve-hour
                // clock and the widest form of each are covered.
                runs.extend((0..24).map(|hour| time_of_day(hour, 5)));
            }
            set_clock(Clock::FromLanguage);
            for run in runs {
                for c in run.chars() {
                    assert!(
                        crate::gpu::corner_can_draw(c),
                        "{language:?} writes a clock as {run:?}, and the corner cannot draw {c:?}"
                    );
                }
            }
        }
        set(Language::British);
    }

    /// A moment carries the day it fell on only when that day is not today.
    ///
    /// What it is for is the note beside the message the light is on in a
    /// conversation: a time on its own is what everybody wants of something
    /// said an hour ago, and a lie about something said last week.
    ///
    /// `now` is stated rather than read, so this says the same thing tomorrow
    /// — and the two moments it compares are three days apart, which is a
    /// different local day in every zone there is however the clocks moved in
    /// between.
    #[test]
    fn a_moment_carries_its_day_only_when_it_is_not_today() {
        set(Language::British);
        const AT: libc::time_t = 1_700_000_000;
        let today = moment_on(AT, Some(AT)).expect("a moment");
        assert!(
            today.with_the_day.is_none(),
            "something said today was given today's date"
        );
        assert!(today.time.contains(':'));

        let older = moment_on(AT, Some(AT + 3 * 86_400)).expect("a moment");
        assert_eq!(older.time, today.time, "the same moment told two times");
        let day = older.with_the_day.expect("the day it was said on");
        assert!(
            day.contains(&older.time),
            "{day:?} was a date with no time in it"
        );
        assert!(day.len() > older.time.len());

        // And it is the session's own clock, not a second one of its own.
        set_clock(Clock::TwelveHour);
        let twelve = moment_on(AT, Some(AT)).expect("a moment");
        assert!(
            twelve.time.ends_with("AM") || twelve.time.ends_with("PM"),
            "{:?} is not on the clock the session is set to",
            twelve.time
        );
        set_clock(Clock::FromLanguage);
    }

    /// The two clocks, and the one the language answers for.
    #[test]
    fn the_clock_is_a_setting_that_falls_back_to_the_language() {
        set(Language::British);
        assert_eq!(clock(), Clock::FromLanguage, "nothing is chosen to start");
        assert!(!twelve_hour(), "and English (UK) writes 20:10");
        assert_eq!(time_of_day(20, 10), "20:10");
        assert_eq!(time_of_day(0, 40), "0:40");

        set(Language::American);
        assert!(twelve_hour(), "English (US) writes 8:10 PM");
        assert_eq!(time_of_day(20, 10), "8:10 PM");
        assert_eq!(time_of_day(0, 40), "12:40 AM", "midnight is twelve");
        assert_eq!(time_of_day(12, 0), "12:00 PM", "and so is noon");
        assert_eq!(time_of_day(11, 59), "11:59 AM");
        // India reads the same clock; the rest of the ten read the other.
        set(Language::Hindi);
        assert!(twelve_hour(), "Hindi writes 8:10 PM");
        assert_eq!(time_of_day(20, 10), "8:10 PM");
        for language in [
            Language::German,
            Language::Portuguese,
            Language::Russian,
            Language::Chinese,
            Language::French,
            Language::Spanish,
        ] {
            set(language);
            assert!(!twelve_hour(), "{language:?} writes 20:10");
            assert_eq!(time_of_day(20, 10), "20:10");
        }

        // The row outranks the language in both directions.
        set_clock(Clock::TwentyFourHour);
        assert_eq!(time_of_day(20, 10), "20:10");
        set(Language::Polish);
        set_clock(Clock::TwelveHour);
        assert_eq!(time_of_day(20, 10), "8:10 PM");

        set_clock(Clock::FromLanguage);
        assert!(!twelve_hour(), "Polish is back to its own clock");
        assert_eq!(Clock::parse("12-hour"), Some(Clock::TwelveHour));
        assert_eq!(Clock::parse("half past"), None);
        for chosen in Clock::CHOICES {
            assert_eq!(Clock::parse(chosen.key()), Some(chosen));
        }
        assert!(!Clock::CHOICES.contains(&Clock::FromLanguage));
        set(Language::British);
    }

    /// The order of a date is the language's, in words and in figures alike.
    #[test]
    fn each_language_writes_a_date_in_its_own_order() {
        set(Language::British);
        assert_eq!(date(16, 8, 2026).as_deref(), Some("16 September 2026"));
        assert_eq!(date_in_figures(16, 9, 2026), "16/09/2026");

        set(Language::American);
        assert_eq!(date(16, 8, 2026).as_deref(), Some("September 16, 2026"));
        assert_eq!(date_in_figures(16, 9, 2026), "09/16/2026");

        set(Language::Polish);
        assert_eq!(date(16, 8, 2026).as_deref(), Some("16 września 2026"));
        assert_eq!(date_in_figures(16, 9, 2026), "16.09.2026");

        // French and Spanish put the day first like the British one, and
        // Spanish fences the month with *de* on both sides.
        set(Language::French);
        assert_eq!(date(16, 8, 2026).as_deref(), Some("16 septembre 2026"));
        assert_eq!(date_in_figures(16, 9, 2026), "16/09/2026");

        set(Language::Spanish);
        assert_eq!(
            date(16, 8, 2026).as_deref(),
            Some("16 de septiembre de 2026")
        );
        assert_eq!(date_in_figures(16, 9, 2026), "16/09/2026");

        // German and Russian point the day and month with full stops, and
        // Russian's month is in the genitive as Polish's is; Portuguese
        // fences the month as Spanish does; Hindi is the British order in
        // its own script; and Chinese goes largest first, year to day, with
        // the month's name being its number and a character.
        set(Language::German);
        assert_eq!(date(16, 8, 2026).as_deref(), Some("16. September 2026"));
        assert_eq!(date_in_figures(16, 9, 2026), "16.09.2026");
        set(Language::Russian);
        assert_eq!(date(16, 8, 2026).as_deref(), Some("16 сентября 2026 г."));
        assert_eq!(date_in_figures(16, 9, 2026), "16.09.2026");
        set(Language::Portuguese);
        assert_eq!(date(16, 8, 2026).as_deref(), Some("16 de setembro de 2026"));
        assert_eq!(date_in_figures(16, 9, 2026), "16/09/2026");
        set(Language::Hindi);
        assert_eq!(date(16, 8, 2026).as_deref(), Some("16 सितंबर 2026"));
        assert_eq!(date_in_figures(16, 9, 2026), "16/09/2026");
        set(Language::Chinese);
        assert_eq!(date(16, 8, 2026).as_deref(), Some("2026年9月16日"));
        assert_eq!(date_in_figures(16, 9, 2026), "2026/09/16");
        set(Language::British);
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
        assert_eq!(supported("ja-JP"), Language::British);
        assert_eq!(spoken_by("pl_PL.UTF-8"), Language::Polish);
        assert_eq!(spoken_by("pl"), Language::Polish);
        // The one English named by its country, and the ones that are not.
        assert_eq!(spoken_by("en_US.UTF-8"), Language::American);
        assert_eq!(spoken_by("en_US"), Language::American);
        assert_eq!(spoken_by("en_GB.UTF-8"), Language::British);
        assert_eq!(spoken_by("en_AU.UTF-8"), Language::British);
        assert_eq!(spoken_by("en"), Language::British);
        assert_eq!(spoken_by("C"), Language::British);
        // The five that followed: a region is never looked at, so Austria
        // reads the German catalog, and the two catalogs named for a region
        // are read by the whole language.
        assert_eq!(spoken_by("de_AT.UTF-8"), Language::German);
        assert_eq!(spoken_by("hi_IN.UTF-8"), Language::Hindi);
        assert_eq!(spoken_by("pt_BR.UTF-8"), Language::Portuguese);
        assert_eq!(spoken_by("pt_PT.UTF-8"), Language::Portuguese);
        assert_eq!(spoken_by("pt"), Language::Portuguese);
        assert_eq!(spoken_by("ru_RU.UTF-8"), Language::Russian);
        assert_eq!(spoken_by("zh_CN.UTF-8"), Language::Chinese);
        assert_eq!(spoken_by("zh_TW.UTF-8"), Language::Chinese);
        assert_eq!(spoken_by("zh-Hans-CN"), Language::Chinese);
        assert_eq!(Language::parse("pt_BR"), Some(Language::Portuguese));
        assert_eq!(Language::parse("zh-CN"), Some(Language::Chinese));
        assert_eq!(
            Language::parse("pt"),
            None,
            "no catalog is named by the bare tag"
        );
        assert_eq!(Language::parse("not-a-language"), None);
        assert_eq!(Language::parse("system"), Some(Language::System));
        // A file from before the two Englishes were told apart.
        assert_eq!(Language::parse("en"), Some(Language::British));
        assert_eq!(Language::parse("en_US"), Some(Language::American));
        for language in Language::CHOICES {
            assert_eq!(Language::parse(language.key()), Some(language));
        }
        assert!(!Language::CHOICES.contains(&Language::System));
        // A shell with nothing chosen speaks one of the two, and names itself
        // by it: the row never has to say "System default".
        set(Language::System);
        assert!(Language::CHOICES.contains(&spoken()));
        assert_eq!(Language::System.name(), spoken().name());
        set(Language::British);
    }

    #[test]
    fn switch_preserves_static_borrows_and_uses_english_fallback() {
        set(Language::British);
        let before = text("language-title");
        set(Language::Polish);
        assert_eq!(before, "Language");
        assert_eq!(text("language-title"), "Język");
        let partial = Catalog::new("pl", "other = Inne\n");
        assert_eq!(
            partial
                .plain
                .get("language-title")
                .or_else(|| BRITISH.plain.get("language-title"))
                .unwrap(),
            "Language"
        );
        set(Language::British);
    }
}
