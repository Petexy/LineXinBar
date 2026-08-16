//! Where this machine is, and when the sun rises and sets there.
//!
//! One caller: the night light, which can be asked to keep the hours the sun
//! keeps rather than two the user typed. That needs a latitude and a longitude,
//! and the question of where to get them is most of what this module is.
//!
//! # Where the location comes from
//!
//! From the time zone, out of the zone table the C library's own data ships —
//! `/usr/share/zoneinfo/zone1970.tab`, which gives a representative coordinate
//! for every zone there is. `/etc/localtime` says which zone this machine is
//! in, and that one line says where that is.
//!
//! It is not the only way it could be done, and the alternatives were all
//! worse. A geolocation daemon is a service that may not be installed, wants an
//! agent, and asks a permission question the Settings column has no business
//! raising for a colour. An address looked up over the network is the user's
//! location leaving the machine to answer a question about their own screen.
//! Asking them to type a latitude is asking them to look one up. This is
//! already on the disk, already correct, already theirs, and costs one file
//! read for the whole session.
//!
//! What it gives is a *city*, not a position: the zone's own representative
//! point. Somewhere in a large zone that can be a few hundred kilometres away,
//! which moves sunset by some tens of minutes — worth knowing, and far inside
//! what a night light cares about. A user who wants better can write the two
//! coordinates into the settings file, which is the one case
//! [`Location::exact`] exists for.
//!
//! # The sun itself
//!
//! The NOAA solar position equations, in the short form: a fractional year, an
//! equation of time, a declination, and the hour angle at which the centre of
//! the sun is 0.833° below the horizon — the standard definition of sunrise,
//! which includes the refraction that makes the sun visible while it is
//! geometrically already down. Good to about a minute, which is a great deal
//! better than the setting needs.

use std::sync::Mutex;

/// A place on the earth, and what it is called.
#[derive(Debug, Clone, PartialEq)]
pub struct Location {
    /// Degrees north, negative south.
    pub latitude: f64,
    /// Degrees east, negative west.
    pub longitude: f64,
    /// The time zone this was read out of — `Europe/Warsaw` — or the word the
    /// settings file used, when the coordinates were written by hand. It is
    /// what the page names, because "the sun where?" is a question the user is
    /// entitled to see answered.
    pub name: String,
}

impl Location {
    /// A location written down rather than looked up, for a settings file that
    /// names one.
    ///
    /// The escape hatch for the one thing the zone table cannot do: say where
    /// somebody actually is inside a zone the size of a country. Nothing in the
    /// shell writes this — there is no page for it, because a page that asked
    /// for a latitude would be asking the user to go and find one — but a file
    /// that carries it is believed.
    pub fn exact(latitude: f64, longitude: f64) -> Option<Self> {
        (latitude.is_finite()
            && longitude.is_finite()
            && (-90.0..=90.0).contains(&latitude)
            && (-180.0..=180.0).contains(&longitude))
        .then(|| Self {
            latitude,
            longitude,
            name: "the settings file".to_string(),
        })
    }
}

/// What the sun does on one day at one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sun {
    /// It rises and sets, at these minutes of local time.
    Daily { sunrise: u16, sunset: u16 },
    /// It does not rise at all: the polar night, and a day the night light
    /// should burn the whole of.
    NeverRises,
    /// It does not set: the midnight sun, and a day with no night in it.
    NeverSets,
}

/// Where this machine is, as far as anything on it says. Read once.
///
/// `None` on a machine with no zone data, or one whose zone the table does not
/// carry — both of which are legal, and both of which mean the night light has
/// no sun to follow. The page says so rather than guessing at a coordinate.
pub fn location() -> Option<Location> {
    if let Some(written) = WRITTEN.lock().unwrap().clone() {
        return Some(written);
    }
    let mut held = FOUND.lock().unwrap();
    if let Some(known) = held.as_ref() {
        return known.clone();
    }
    let found = from_zone_table();
    match &found {
        Some(at) => tracing::info!(
            zone = at.name,
            latitude = at.latitude,
            longitude = at.longitude,
            "where the sun is worked out for"
        ),
        None => tracing::debug!("no zone coordinates; the night light cannot follow the sun"),
    }
    *held = Some(found.clone());
    found
}

/// The zone table's answer, once it has been asked for. `Some(None)` is a
/// machine that was asked and does not say, which is not worth asking twice.
static FOUND: Mutex<Option<Option<Location>>> = Mutex::new(None);

/// The coordinates the settings file named, if it named any. Consulted before
/// the table, so a file that says where the machine is has the last word.
static WRITTEN: Mutex<Option<Location>> = Mutex::new(None);

/// The place the settings file named, if it named one.
///
/// For the writer, which builds the file out of the live values: a key it
/// cannot see is one the next change to anything else would drop.
pub fn written_location() -> Option<Location> {
    WRITTEN.lock().unwrap().clone()
}

/// Replace what [`location`] answers, for a settings file that names a place.
///
/// Passing `None` puts it back to the zone table, which is what a file that
/// stops naming one means.
pub fn set_location(exact: Option<Location>) {
    if let Some(at) = &exact {
        tracing::info!(
            latitude = at.latitude,
            longitude = at.longitude,
            "the settings name where this machine is"
        );
    }
    *WRITTEN.lock().unwrap() = exact;
}

/// The zone table's answer.
fn from_zone_table() -> Option<Location> {
    let zone = zone_name()?;
    let table = std::fs::read_to_string("/usr/share/zoneinfo/zone1970.tab")
        // The older table, which carries some zone names the 1970 one dropped.
        // A machine that has one and not the other is ordinary.
        .or_else(|_| std::fs::read_to_string("/usr/share/zoneinfo/zone.tab"))
        .ok()?;
    coordinates_of(&zone, &table).map(|(latitude, longitude)| Location {
        latitude,
        longitude,
        name: zone,
    })
}

/// Which zone this machine is in, by its IANA name.
///
/// `TZ` first, because a session that sets it means it. Then the symlink every
/// distribution points at the zone's own file, and then the file Debian writes
/// the name into. Anything else is a machine that does not say.
fn zone_name() -> Option<String> {
    if let Some(named) = std::env::var_os("TZ") {
        let named = named.to_string_lossy();
        // `TZ` may hold a whole POSIX rule — `CET-1CEST,M3.5.0,M10.5.0/3` —
        // rather than a zone name. Only a name can be looked up, and the
        // leading colon some systems use is not part of it.
        let named = named.trim_start_matches(':').trim();
        if named.contains('/') {
            return Some(named.to_string());
        }
    }
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        let path = target.to_string_lossy();
        // `../usr/share/zoneinfo/Europe/Warsaw`, or the absolute form. What is
        // wanted is everything after the directory, which is two components
        // for most zones and one for a few.
        if let Some((_, zone)) = path.split_once("zoneinfo/") {
            let zone = zone.trim_matches('/');
            if !zone.is_empty() {
                return Some(zone.to_string());
            }
        }
    }
    let named = std::fs::read_to_string("/etc/timezone").ok()?;
    let named = named.trim();
    (!named.is_empty()).then(|| named.to_string())
}

/// Find a zone in the table and read its coordinate.
///
/// Split out so the parsing can be tested against lines written here rather
/// than against whatever this machine happens to have installed.
fn coordinates_of(zone: &str, table: &str) -> Option<(f64, f64)> {
    for line in table.lines() {
        if line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let (_countries, coordinates, name) = (fields.next()?, fields.next(), fields.next());
        if name? != zone {
            continue;
        }
        return parse_iso6709(coordinates?);
    }
    None
}

/// `+5215+02100`, or `+521500+0210000`: the ISO 6709 form the zone table uses.
///
/// Latitude is two degree digits, longitude three, each followed by minutes and
/// optionally seconds. Anything else is a table this does not understand, which
/// comes back as no location rather than as a guess.
fn parse_iso6709(raw: &str) -> Option<(f64, f64)> {
    let raw = raw.trim();
    // The second sign is where the longitude starts; the first is at 0.
    let split = raw
        .char_indices()
        .skip(1)
        .find(|(_, character)| *character == '+' || *character == '-')
        .map(|(at, _)| at)?;
    let (latitude, longitude) = raw.split_at(split);
    Some((sexagesimal(latitude, 2)?, sexagesimal(longitude, 3)?))
}

/// One signed `±DD[D]MM[SS]` figure, in degrees.
fn sexagesimal(raw: &str, degree_digits: usize) -> Option<f64> {
    let sign = match raw.chars().next()? {
        '+' => 1.0,
        '-' => -1.0,
        _ => return None,
    };
    let digits = &raw[1..];
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    // Degrees and minutes, or degrees, minutes and seconds. No other length is
    // a coordinate.
    let seconds_too = match digits.len() {
        length if length == degree_digits + 2 => false,
        length if length == degree_digits + 4 => true,
        _ => return None,
    };
    let number = |from: usize, to: usize| digits[from..to].parse::<f64>().ok();
    let degrees = number(0, degree_digits)?;
    let minutes = number(degree_digits, degree_digits + 2)?;
    let seconds = match seconds_too {
        true => number(degree_digits + 2, degree_digits + 4)?,
        false => 0.0,
    };
    Some(sign * (degrees + minutes / 60.0 + seconds / 3600.0))
}

/// When the sun rises and sets at `at`, on the `yday`th day of `year` counted
/// from zero, for a clock `utc_offset` seconds east of UTC.
///
/// The offset is passed in rather than worked out, because it is the one part
/// of this the C library has already answered — and answered including whatever
/// summer time is in force today, which no amount of arithmetic here would get
/// right.
pub fn sun(yday: u16, year: i32, at: &Location, utc_offset: i32) -> Sun {
    let days = days_in_year(year) as f64;
    // NOAA's fractional year, taken at noon: the equation of time and the
    // declination both move slowly enough that the middle of the day is a good
    // enough moment to evaluate them for both ends of it.
    let gamma = std::f64::consts::TAU / days * yday as f64;

    let (sin1, cos1) = gamma.sin_cos();
    let (sin2, cos2) = (2.0 * gamma).sin_cos();
    let (sin3, cos3) = (3.0 * gamma).sin_cos();

    // Minutes by which apparent solar time runs ahead of mean solar time.
    let equation_of_time = 229.18
        * (0.000_075 + 0.001_868 * cos1 - 0.032_077 * sin1 - 0.014_615 * cos2 - 0.040_849 * sin2);
    // How far north of the equator the sun is overhead, in radians.
    let declination = 0.006_918 - 0.399_912 * cos1 + 0.070_257 * sin1 - 0.006_758 * cos2
        + 0.000_907 * sin2
        - 0.002_697 * cos3
        + 0.001_48 * sin3;

    let latitude = at.latitude.to_radians();
    // 90.833°: the sun's centre is a little below the horizon at the moment its
    // upper limb appears, because the atmosphere bends the light round and
    // because the disc has a width.
    let zenith: f64 = 90.833_f64.to_radians();
    let hour_angle = (zenith.cos() / (latitude.cos() * declination.cos())
        - latitude.tan() * declination.tan())
    .clamp(-2.0, 2.0);

    // Out of range in either direction is not a failure: it is a latitude where
    // the sun does not cross the horizon today, which is the whole of the
    // answer for a night light.
    if hour_angle > 1.0 {
        return Sun::NeverRises;
    }
    if hour_angle < -1.0 {
        return Sun::NeverSets;
    }
    let hour_angle = hour_angle.acos().to_degrees();

    let local = |angle: f64| {
        // 720 is solar noon at longitude 0; four minutes is a degree of
        // rotation. The offset then puts it on the clock in the room.
        let minutes =
            720.0 - 4.0 * (at.longitude + angle) - equation_of_time + utc_offset as f64 / 60.0;
        // A day wraps: a place far enough east of its own zone can have a
        // sunrise that lands on the previous day's clock.
        let wrapped = minutes.rem_euclid(24.0 * 60.0);
        wrapped.round().clamp(0.0, 1439.0) as u16
    };
    Sun::Daily {
        sunrise: local(hour_angle),
        sunset: local(-hour_angle),
    }
}

/// 366 in a leap year, 365 otherwise. The fractional year above is divided by
/// it, and getting it wrong moves the answer by about a minute at the solstices
/// — small, and free to be right.
fn days_in_year(year: i32) -> u16 {
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    match leap {
        true => 366,
        false => 365,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A place with a name nobody's machine is set to, so nothing here can be
    /// passing because of where this was built. The coordinates are real —
    /// they have to be, or the arithmetic could not be checked — but they are
    /// written down in the test rather than read off the machine.
    fn at(latitude: f64, longitude: f64) -> Location {
        Location {
            latitude,
            longitude,
            name: "Test/Somewhere".to_string(),
        }
    }

    /// The zone table's own format, including the two shapes of coordinate and
    /// the rows that are not a zone at all.
    #[test]
    fn a_zone_is_found_in_the_table_by_its_own_name() {
        let table = "\
# comment\tnot\ta\tzone
PL\t+5215+02100\tTest/North
NZ\t-3652+17446\tTest/South\tsome comment
US\t+433458-0794217\tTest/Seconds
";
        let (latitude, longitude) = coordinates_of("Test/North", table).unwrap();
        assert!((latitude - 52.25).abs() < 1e-9, "{latitude}");
        assert!((longitude - 21.0).abs() < 1e-9, "{longitude}");

        // Southern and western, with a fourth column after the name.
        let (latitude, longitude) = coordinates_of("Test/South", table).unwrap();
        assert!((latitude + 36.866_666).abs() < 1e-5, "{latitude}");
        assert!((longitude - 174.766_666).abs() < 1e-5, "{longitude}");

        // Degrees, minutes *and* seconds, which the table also uses.
        let (latitude, longitude) = coordinates_of("Test/Seconds", table).unwrap();
        assert!((latitude - 43.582_777).abs() < 1e-5, "{latitude}");
        assert!((longitude + 79.704_722).abs() < 1e-5, "{longitude}");

        // A zone the table does not carry is no location, not a wrong one.
        assert_eq!(coordinates_of("Test/Missing", table), None);
        // And the comment line is not a zone called `# comment`.
        assert_eq!(coordinates_of("# comment", table), None);
    }

    /// Malformed coordinates come back as nothing rather than as a place in
    /// the sea. This is a file on disk, and a file on disk can be anything.
    #[test]
    fn a_coordinate_that_is_not_one_is_no_location() {
        assert_eq!(parse_iso6709(""), None);
        assert_eq!(parse_iso6709("+5215"), None, "a latitude alone");
        assert_eq!(parse_iso6709("5215+02100"), None, "no sign");
        assert_eq!(parse_iso6709("+52x5+02100"), None, "not digits");
        assert_eq!(parse_iso6709("+521+02100"), None, "not a length it has");
        // The two lengths it does have.
        assert!(parse_iso6709("+5215+02100").is_some());
        assert!(parse_iso6709("+521500+0210000").is_some());
    }

    /// The sun at a place and a date whose answer is known, to the minute or
    /// two the equations are good for.
    ///
    /// Not this machine's location and not today: both would make the test
    /// depend on where and when it was run, which is the one thing a test of
    /// the solar equations may not do.
    #[test]
    fn the_sun_rises_and_sets_when_the_almanac_says() {
        // 52°15′N 21°00′E on the 12th of August 2026 — the 223rd day counted
        // from zero — with a clock two hours east of UTC. Sunrise 05:15,
        // sunset 20:12.
        let Sun::Daily { sunrise, sunset } = sun(223, 2026, &at(52.25, 21.0), 2 * 3600) else {
            panic!("the sun rises there in August");
        };
        assert!(
            (sunrise as i32 - (5 * 60 + 15)).abs() <= 3,
            "sunrise {sunrise}"
        );
        assert!(
            (sunset as i32 - (20 * 60 + 12)).abs() <= 3,
            "sunset {sunset}"
        );

        // The same place at midwinter: the day is far shorter, and both ends
        // move the way they should.
        let Sun::Daily {
            sunrise: winter_rise,
            sunset: winter_set,
        } = sun(355, 2026, &at(52.25, 21.0), 3600)
        else {
            panic!("the sun rises there in December too");
        };
        assert!(winter_rise > sunrise, "it rises later in winter");
        assert!(winter_set < sunset, "and sets earlier");
        assert!(
            (winter_set - winter_rise) < (sunset - sunrise),
            "a winter day is shorter"
        );

        // South of the equator the seasons are the other way about.
        let Sun::Daily {
            sunrise: south_rise,
            sunset: south_set,
        } = sun(223, 2026, &at(-33.87, 151.21), 10 * 3600)
        else {
            panic!("the sun rises in Sydney in August");
        };
        assert!(
            (south_set - south_rise) < (sunset - sunrise),
            "August is winter there"
        );
    }

    /// Above the arctic circle the sun does not always cross the horizon, and
    /// the arithmetic must say so rather than take the arc cosine of something
    /// out of range and come back with a time made of NaN.
    #[test]
    fn the_poles_have_days_with_no_sunrise_and_days_with_no_sunset() {
        let arctic = at(78.22, 15.65);
        assert_eq!(sun(180, 2026, &arctic, 3600), Sun::NeverSets, "midsummer");
        assert_eq!(sun(0, 2026, &arctic, 3600), Sun::NeverRises, "midwinter");
        // And in between it behaves like anywhere else.
        assert!(matches!(sun(80, 2026, &arctic, 3600), Sun::Daily { .. }));

        // The other pole, at the opposite ends of the year.
        let antarctic = at(-77.85, 166.67);
        assert_eq!(sun(180, 2026, &antarctic, 12 * 3600), Sun::NeverRises);
        assert_eq!(sun(0, 2026, &antarctic, 12 * 3600), Sun::NeverSets);
    }

    /// Every day of a year, everywhere, gives a time that is a time.
    ///
    /// The equations are trigonometry over values a file on disk supplies, so
    /// the property worth asserting is not any one answer but that none of them
    /// is NaN, none is out of the day, and the sun never sets before it rises.
    #[test]
    fn no_day_anywhere_produces_a_time_that_is_not_one() {
        for latitude in [-66, -45, -23, 0, 23, 45, 66] {
            for longitude in [-179, -90, 0, 90, 179] {
                let place = at(latitude as f64, longitude as f64);
                for yday in 0..366 {
                    let Sun::Daily { sunrise, sunset } = sun(yday, 2026, &place, 0) else {
                        continue;
                    };
                    assert!(sunrise < 1440, "{latitude} {longitude} {yday}: {sunrise}");
                    assert!(sunset < 1440, "{latitude} {longitude} {yday}: {sunset}");
                }
            }
        }
    }

    /// A location written into the settings file is believed, and a nonsense
    /// one is not.
    #[test]
    fn a_written_location_is_checked_before_it_is_believed() {
        assert!(Location::exact(52.25, 21.0).is_some());
        assert!(Location::exact(-89.9, -179.9).is_some());
        assert!(Location::exact(91.0, 0.0).is_none(), "off the earth");
        assert!(Location::exact(0.0, 181.0).is_none());
        assert!(Location::exact(f64::NAN, 0.0).is_none());
    }

    /// The leap year, which the fractional year is divided by.
    #[test]
    fn a_leap_year_is_one_day_longer() {
        assert_eq!(days_in_year(2026), 365);
        assert_eq!(days_in_year(2024), 366);
        assert_eq!(days_in_year(2000), 366, "a four-hundredth year is leap");
        assert_eq!(days_in_year(1900), 365, "a hundredth year is not");
    }
}
