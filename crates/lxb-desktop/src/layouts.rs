//! Which keyboard arrangements this machine can be set to, and how they are
//! filed.
//!
//! The list is **read off the machine** rather than written down here. Every
//! layout xkb can compile is described in `rules/evdev.xml` under the xkb
//! config root — the same registry `setxkbmap` and every desktop's keyboard
//! page read — so what this shell offers is exactly what libxkbcommon on this
//! machine will accept. A table of layouts in the source would be a promise
//! about somebody else's xkeyboard-config, and the first one it got wrong
//! would be a setting that applies to nothing.
//!
//! What is written down here is the one thing that registry does not carry:
//! **where a country is.** Every layout names the countries that claim it, as
//! ISO 3166-1 alpha-2 codes, and nothing anywhere on the machine says that PL
//! is in Europe. So [`COUNTRIES`] does, for all 249 codes rather than for the
//! hundred-odd that have a layout today — a code this table does not know is
//! filed under [`Continent::Other`] and shown by its code, which keeps a
//! layout reachable rather than losing it.
//!
//! ## The shape of the tree
//!
//! Continent, then country, then every arrangement that country has — the base
//! layout and its variants in one list, because "Polish" and "Polish (QWERTZ)"
//! are two answers to one question and a column holding only the first would
//! be a column nobody could finish. 598 arrangements on this machine's
//! xkeyboard-config, which is what the tree is for: as one list it is
//! unreadable, and the deepest column of the tree is nine rows.
//!
//! A layout claimed by several countries is under each of them, which is what
//! its country list means: Arabic is the keyboard of nineteen countries and
//! belongs in all nineteen columns.

use std::collections::BTreeMap;
use std::sync::OnceLock;

/// One arrangement a keyboard can be set to: an xkb layout, and one of that
/// layout's variants where it is one.
///
/// The base layout is one of these too, with an empty variant. It is not the
/// odd one out — `pl` and `pl (qwertz)` are both things xkb is set to, they are
/// both called something, and the page offers them side by side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The xkb layout name: `pl`, `us`, `ara`.
    pub layout: String,
    /// The xkb variant name, empty for the layout's own arrangement.
    pub variant: String,
    /// What xkeyboard-config calls it: "Polish (QWERTZ)".
    pub name: String,
    /// The countries that claim it, as ISO 3166-1 alpha-2 codes. Empty for the
    /// three that belong to no country — Braille, Esperanto, and a layout the
    /// machine's owner wrote themselves.
    pub countries: Vec<String>,
}

impl Layout {
    /// How the two names are written where both have to fit in one string: the
    /// layout, and the variant in brackets after it where there is one.
    ///
    /// The form `setxkbmap -query` prints and the form the settings file
    /// carries, so a user who has looked their layout up anywhere else
    /// recognises what is written down. See [`from_key`].
    pub fn key(&self) -> String {
        match self.variant.is_empty() {
            true => self.layout.clone(),
            false => format!("{} ({})", self.layout, self.variant),
        }
    }

    /// Whether this is the arrangement those two xkb names describe.
    pub fn is(&self, layout: &str, variant: &str) -> bool {
        self.layout == layout && self.variant == variant
    }
}

/// Read `layout (variant)` back into the two names it is written from.
///
/// Deliberately tolerant of a hand-written file: anything that is not the
/// bracketed form is taken as a bare layout, because that is what somebody
/// typing `pl` into the settings file means. Whether the result names an
/// arrangement this machine has is a separate question, and one the caller
/// asks — see [`find`].
pub fn from_key(key: &str) -> (String, String) {
    let key = key.trim();
    match key.split_once('(') {
        Some((layout, rest)) => (
            layout.trim().to_string(),
            rest.trim_end().trim_end_matches(')').trim().to_string(),
        ),
        None => (key.to_string(), String::new()),
    }
}

/// Where in the world a country is, which is the top of the tree.
///
/// Six of them and a seventh that is not a continent at all. Antarctica is
/// among them because it is among the ISO codes, and it has never had a
/// keyboard layout — [`Registry::continents`] leaves out whichever of these
/// nothing is filed under, so it costs a row on no machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Continent {
    Africa,
    Antarctica,
    Asia,
    Europe,
    NorthAmerica,
    Oceania,
    SouthAmerica,
    /// Not a continent: the arrangements that belong to no country, and the
    /// countries this shell's own table has never heard of.
    ///
    /// Braille and Esperanto are the two every machine has — neither is any
    /// country's keyboard, and both are real answers somebody might want — and
    /// a `custom` layout appears here on a machine whose owner has written one
    /// into their xkb config. Last in the column, because it is the group for
    /// what the other seven could not hold.
    Other,
}

impl Continent {
    /// In the order the column lists them: alphabetically, and the group that
    /// is not a continent at the end.
    pub const ALL: [Continent; 8] = [
        Continent::Africa,
        Continent::Antarctica,
        Continent::Asia,
        Continent::Europe,
        Continent::NorthAmerica,
        Continent::Oceania,
        Continent::SouthAmerica,
        Continent::Other,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Continent::Africa => crate::i18n::text("shell-africa"),
            Continent::Antarctica => crate::i18n::text("shell-antarctica"),
            Continent::Asia => crate::i18n::text("shell-asia"),
            Continent::Europe => crate::i18n::text("shell-europe"),
            Continent::NorthAmerica => crate::i18n::text("shell-north-america"),
            Continent::Oceania => crate::i18n::text("shell-oceania"),
            Continent::SouthAmerica => crate::i18n::text("shell-south-america"),
            Continent::Other => crate::i18n::text("shell-other"),
        }
    }
}

/// One country with at least one keyboard layout to its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Country {
    /// The ISO 3166-1 alpha-2 code the registry files layouts under.
    pub code: String,
    /// What to call it. The code itself where this shell's table has never
    /// heard of it, which is honest and reachable — see [`country_name`].
    pub name: String,
}

/// Every layout this machine has, and the tree they are shown as.
#[derive(Debug, Default)]
pub struct Registry {
    layouts: Vec<Layout>,
}

impl Registry {
    /// The continents anything is filed under, in [`Continent::ALL`] order.
    ///
    /// Whichever of the eight has nothing under it is left out rather than
    /// shown empty: a column with nothing in it is the one shape this bar
    /// cannot draw, so a row leading to one would be a row that cannot be
    /// pressed.
    pub fn continents(&self) -> Vec<Continent> {
        Continent::ALL
            .into_iter()
            .filter(|continent| !self.countries_in(*continent).is_empty())
            .collect()
    }

    /// The countries in one continent that have a layout, by name.
    ///
    /// Alphabetically, which is the only order a list of countries has. The
    /// group that is not a continent holds exactly one country that is not one
    /// either — see [`NOWHERE`].
    pub fn countries_in(&self, continent: Continent) -> Vec<Country> {
        // Keyed by the collating form of the name, not the name: "Åland" and
        // "Łotwa" are read as A and L by the people looking for them, and
        // byte order would put both after Z.
        let mut seen: BTreeMap<crate::i18n::SortKey, Country> = BTreeMap::new();
        for layout in &self.layouts {
            for code in self.codes_of(layout) {
                if place_of(&code) != continent {
                    continue;
                }
                let name = country_name(&code);
                seen.insert(crate::i18n::sort_key(&name), Country { code, name });
            }
        }
        seen.into_values().collect()
    }

    /// Every arrangement one country claims, in the registry's own order.
    ///
    /// **Not sorted.** xkeyboard-config lists a layout before its variants, so
    /// the first row of the column is the arrangement that country's keyboards
    /// actually have and the rest are the departures from it. Sorted by name
    /// they would interleave — "Polish (Dvorak)" above "Polish" — and the
    /// answer nearly everybody wants would stop being the first one.
    pub fn layouts_in(&self, code: &str) -> Vec<&Layout> {
        self.layouts
            .iter()
            .filter(|layout| self.codes_of(layout).iter().any(|held| held == code))
            .collect()
    }

    /// The arrangement those two xkb names describe, if this machine has it.
    pub fn find(&self, layout: &str, variant: &str) -> Option<&Layout> {
        self.layouts.iter().find(|held| held.is(layout, variant))
    }

    /// Every arrangement whose name, xkb name or country answers to `query`.
    ///
    /// The search behind the field at the head of every column of this page,
    /// and it searches the *whole* tree from wherever it is typed into — a
    /// field that narrowed six continent names would be a row that does
    /// nothing. Matching the country as well as the name is what makes
    /// "Poland" find the Polish layouts, which is the word somebody who does
    /// not know their layout is called "Polish (QWERTZ)" will reach for.
    ///
    /// Case-insensitively and by substring, which is the rule every other
    /// field in this shell searches by.
    pub fn search(&self, query: &str) -> Vec<&Layout> {
        let wanted = query.trim().to_lowercase();
        if wanted.is_empty() {
            return Vec::new();
        }
        self.layouts
            .iter()
            .filter(|layout| {
                layout.name.to_lowercase().contains(&wanted)
                    || layout.key().to_lowercase().contains(&wanted)
                    || self
                        .codes_of(layout)
                        .iter()
                        .any(|code| country_name(code).to_lowercase().contains(&wanted))
            })
            .collect()
    }

    /// What to say under a found row about where it lives.
    ///
    /// One country, and it is named with its continent, because that is the
    /// path the user would have walked to reach it. Several, and the count is
    /// the honest answer: Arabic is the keyboard of nineteen countries across
    /// two continents, and picking one of them to print would be inventing a
    /// home for it. None, and that is what it says.
    pub fn whereabouts(&self, layout: &Layout) -> String {
        match layout.countries.as_slice() {
            // Not "Other · No country", which is the path but reads as a
            // country called "No country" on a continent called "Other". What
            // is true of Braille is that no country has it.
            [] => NO_COUNTRY.to_string(),
            [only] => format!("{} · {}", place_of(only).title(), country_name(only)),
            many => {
                crate::message!("count-countries", "count" => many.len())
            }
        }
    }

    /// How many arrangements this machine has altogether, which is what the
    /// search field counts against.
    pub fn len(&self) -> usize {
        self.layouts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.layouts.is_empty()
    }

    /// The codes a layout is filed under, with the one that stands for nowhere
    /// substituted where it has none.
    ///
    /// So that "belongs to no country" is a country like any other as far as
    /// the tree is concerned, and Braille needs no special case in three
    /// places. See [`NOWHERE`].
    fn codes_of(&self, layout: &Layout) -> Vec<String> {
        match layout.countries.is_empty() {
            true => vec![NOWHERE.to_string()],
            false => layout.countries.clone(),
        }
    }
}

/// The code standing for "no country at all", which no ISO 3166 list uses.
///
/// `ZZ` is reserved by the standard for exactly this — a user-assigned code
/// that will never be a real country — so nothing xkeyboard-config could
/// one day add can collide with it.
const NOWHERE: &str = "ZZ";

/// What the group of layouts belonging to no country is called, where a
/// country's name is what is wanted.
const NO_COUNTRY: &str = "No country";

/// This machine's layout registry, read once.
///
/// Once because it is a file on the disk that changes when a package is
/// installed, and a session does not outlive that: a shell that re-read it per
/// frame would be opening a 700 KB XML file to draw a column.
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Registry {
        layouts: read_registry(),
    })
}

/// Where xkb keeps its rules, honouring the override libxkbcommon itself
/// honours.
///
/// `XKB_CONFIG_ROOT` first, because that is the variable every xkb tool reads
/// and the one an unusual install sets — a shell that ignored it would offer a
/// list of layouts the compositor beside it cannot compile. The standard root
/// otherwise, which is where every distribution puts xkeyboard-config.
fn config_roots() -> Vec<std::path::PathBuf> {
    if let Some(root) = std::env::var_os("XKB_CONFIG_ROOT") {
        return vec![std::path::PathBuf::from(root)];
    }
    ["/usr/share/X11/xkb", "/usr/local/share/X11/xkb"]
        .iter()
        .map(std::path::PathBuf::from)
        .collect()
}

/// Read the first registry any of the roots has.
///
/// `evdev.xml` is the one that describes what a Linux machine's keyboards
/// actually are; `base.xml` is the older name for the same file and is read
/// where a stripped install has only that. Nothing at all is not an error the
/// shell can do anything about — the page says so and offers no rows, which is
/// better than a list of layouts that would fail to compile.
fn read_registry() -> Vec<Layout> {
    for root in config_roots() {
        for name in ["evdev.xml", "base.xml"] {
            let path = root.join("rules").join(name);
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            match parse(&text) {
                Ok(layouts) if !layouts.is_empty() => {
                    tracing::info!(path = %path.display(), layouts = layouts.len(), "keyboard layouts");
                    return layouts;
                }
                Ok(_) => tracing::warn!(path = %path.display(), "the layout registry is empty"),
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "the layout registry would not parse")
                }
            }
        }
    }
    tracing::warn!("no xkb layout registry on this machine");
    Vec::new()
}

/// Every arrangement one registry describes, flattened.
///
/// A pure function of the file's text, which is what makes the tree above
/// testable without an xkeyboard-config to read.
pub fn parse(text: &str) -> Result<Vec<Layout>, roxmltree::Error> {
    // **The DTD has to be allowed.** Every registry xkeyboard-config ships
    // opens `<!DOCTYPE xkbConfigRegistry SYSTEM "xkb.dtd">`, and roxmltree
    // refuses a document with one unless it is asked — so the default parser
    // reads not a single layout on any machine, while the fixtures in this
    // file's own tests, which have no doctype, go on passing. That is exactly
    // how it shipped broken once.
    //
    // Nothing is fetched to satisfy it: roxmltree does not resolve external
    // entities, and its billion-laughs guard stays on. What is being allowed is
    // the declaration, not a document that reaches off the disk.
    let options = roxmltree::ParsingOptions {
        allow_dtd: true,
        ..roxmltree::ParsingOptions::default()
    };
    let document = roxmltree::Document::parse_with_options(text, options)?;
    let mut layouts = Vec::new();
    let Some(list) = document
        .descendants()
        .find(|node| node.has_tag_name("layoutList"))
    else {
        return Ok(layouts);
    };
    for entry in list.children().filter(|node| node.has_tag_name("layout")) {
        let Some(item) = config_item(entry) else {
            continue;
        };
        let (Some(name), Some(description)) = (text_of(item, "name"), text_of(item, "description"))
        else {
            continue;
        };
        let countries = countries_of(item);
        layouts.push(Layout {
            layout: name.clone(),
            variant: String::new(),
            name: description,
            countries: countries.clone(),
        });
        let variants = entry
            .children()
            .find(|node| node.has_tag_name("variantList"))
            .into_iter()
            .flat_map(|list| list.children().filter(|node| node.has_tag_name("variant")));
        for variant in variants {
            let Some(item) = config_item(variant) else {
                continue;
            };
            let (Some(variant_name), Some(description)) =
                (text_of(item, "name"), text_of(item, "description"))
            else {
                continue;
            };
            // A variant's own country list where it has one, and the layout's
            // otherwise. Three of the five hundred variants on this machine
            // carry one, and each of those is a variant of one country's layout
            // that belongs to another.
            let own = countries_of(item);
            layouts.push(Layout {
                layout: name.clone(),
                variant: variant_name,
                name: description,
                countries: match own.is_empty() {
                    true => countries.clone(),
                    false => own,
                },
            });
        }
    }
    Ok(layouts)
}

fn config_item<'a, 'i>(node: roxmltree::Node<'a, 'i>) -> Option<roxmltree::Node<'a, 'i>> {
    node.children().find(|node| node.has_tag_name("configItem"))
}

fn text_of(item: roxmltree::Node, tag: &str) -> Option<String> {
    let text = item
        .children()
        .find(|node| node.has_tag_name(tag))?
        .text()?
        .trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn countries_of(item: roxmltree::Node) -> Vec<String> {
    item.children()
        .filter(|node| node.has_tag_name("countryList"))
        .flat_map(|list| {
            list.children()
                .filter(|node| node.has_tag_name("iso3166Id"))
        })
        .filter_map(|node| node.text())
        .map(|code| code.trim().to_uppercase())
        .filter(|code| !code.is_empty())
        .collect()
}

/// What to call a country, given the code a layout is filed under.
///
/// The code itself where this table has never heard of it. A layout for a
/// country xkeyboard-config knows about and this shell does not is a layout
/// that is still reachable, listed under [`Continent::Other`] by its two
/// letters — which is a worse row than "Poland" and an immeasurably better one
/// than no row at all.
pub fn country_name(code: &str) -> String {
    if code == NOWHERE {
        return crate::i18n::builtin(NO_COUNTRY).to_string();
    }
    COUNTRIES
        .iter()
        .find(|(known, _, _)| *known == code)
        .map(|(_, name, _)| crate::i18n::builtin(name).to_string())
        .unwrap_or_else(|| code.to_string())
}

/// Which continent a country is on.
fn place_of(code: &str) -> Continent {
    if code == NOWHERE {
        return Continent::Other;
    }
    COUNTRIES
        .iter()
        .find(|(known, _, _)| *known == code)
        .map(|(_, _, place)| *place)
        .unwrap_or(Continent::Other)
}

/// Every ISO 3166-1 alpha-2 code, what to call it, and where it is.
///
/// The one thing the xkb registry does not carry and cannot be read off the
/// machine: it files layouts under country codes and says nothing about where
/// those countries are.
///
/// All 249 of them rather than the hundred-odd that have a layout today,
/// because the registry is a file that grows: a code left out would be a
/// layout filed under Other on the day xkeyboard-config added it, and the
/// difference between the two lists is a hundred and twenty lines.
///
/// The short name people use, not the standard's own: "Bolivia" and not
/// "Bolivia, Plurinational State of", "South Korea" and not "Korea, Republic
/// of". This is a list somebody reads down looking for their own country.
#[rustfmt::skip]
const COUNTRIES: &[(&str, &str, Continent)] = &[
    ("AD", "Andorra", Continent::Europe),
    ("AE", "United Arab Emirates", Continent::Asia),
    ("AF", "Afghanistan", Continent::Asia),
    ("AG", "Antigua and Barbuda", Continent::NorthAmerica),
    ("AI", "Anguilla", Continent::NorthAmerica),
    ("AL", "Albania", Continent::Europe),
    ("AM", "Armenia", Continent::Asia),
    ("AO", "Angola", Continent::Africa),
    ("AQ", "Antarctica", Continent::Antarctica),
    ("AR", "Argentina", Continent::SouthAmerica),
    ("AS", "American Samoa", Continent::Oceania),
    ("AT", "Austria", Continent::Europe),
    ("AU", "Australia", Continent::Oceania),
    ("AW", "Aruba", Continent::NorthAmerica),
    ("AX", "Åland Islands", Continent::Europe),
    ("AZ", "Azerbaijan", Continent::Asia),
    ("BA", "Bosnia and Herzegovina", Continent::Europe),
    ("BB", "Barbados", Continent::NorthAmerica),
    ("BD", "Bangladesh", Continent::Asia),
    ("BE", "Belgium", Continent::Europe),
    ("BF", "Burkina Faso", Continent::Africa),
    ("BG", "Bulgaria", Continent::Europe),
    ("BH", "Bahrain", Continent::Asia),
    ("BI", "Burundi", Continent::Africa),
    ("BJ", "Benin", Continent::Africa),
    ("BL", "Saint Barthélemy", Continent::NorthAmerica),
    ("BM", "Bermuda", Continent::NorthAmerica),
    ("BN", "Brunei", Continent::Asia),
    ("BO", "Bolivia", Continent::SouthAmerica),
    ("BQ", "Caribbean Netherlands", Continent::NorthAmerica),
    ("BR", "Brazil", Continent::SouthAmerica),
    ("BS", "Bahamas", Continent::NorthAmerica),
    ("BT", "Bhutan", Continent::Asia),
    ("BV", "Bouvet Island", Continent::Antarctica),
    ("BW", "Botswana", Continent::Africa),
    ("BY", "Belarus", Continent::Europe),
    ("BZ", "Belize", Continent::NorthAmerica),
    ("CA", "Canada", Continent::NorthAmerica),
    ("CC", "Cocos Islands", Continent::Oceania),
    ("CD", "Congo (Kinshasa)", Continent::Africa),
    ("CF", "Central African Republic", Continent::Africa),
    ("CG", "Congo (Brazzaville)", Continent::Africa),
    ("CH", "Switzerland", Continent::Europe),
    ("CI", "Côte d'Ivoire", Continent::Africa),
    ("CK", "Cook Islands", Continent::Oceania),
    ("CL", "Chile", Continent::SouthAmerica),
    ("CM", "Cameroon", Continent::Africa),
    ("CN", "China", Continent::Asia),
    ("CO", "Colombia", Continent::SouthAmerica),
    ("CR", "Costa Rica", Continent::NorthAmerica),
    ("CU", "Cuba", Continent::NorthAmerica),
    ("CV", "Cabo Verde", Continent::Africa),
    ("CW", "Curaçao", Continent::NorthAmerica),
    ("CX", "Christmas Island", Continent::Oceania),
    ("CY", "Cyprus", Continent::Asia),
    ("CZ", "Czechia", Continent::Europe),
    ("DE", "Germany", Continent::Europe),
    ("DJ", "Djibouti", Continent::Africa),
    ("DK", "Denmark", Continent::Europe),
    ("DM", "Dominica", Continent::NorthAmerica),
    ("DO", "Dominican Republic", Continent::NorthAmerica),
    ("DZ", "Algeria", Continent::Africa),
    ("EC", "Ecuador", Continent::SouthAmerica),
    ("EE", "Estonia", Continent::Europe),
    ("EG", "Egypt", Continent::Africa),
    ("EH", "Western Sahara", Continent::Africa),
    ("ER", "Eritrea", Continent::Africa),
    ("ES", "Spain", Continent::Europe),
    ("ET", "Ethiopia", Continent::Africa),
    ("FI", "Finland", Continent::Europe),
    ("FJ", "Fiji", Continent::Oceania),
    ("FK", "Falkland Islands", Continent::SouthAmerica),
    ("FM", "Micronesia", Continent::Oceania),
    ("FO", "Faroe Islands", Continent::Europe),
    ("FR", "France", Continent::Europe),
    ("GA", "Gabon", Continent::Africa),
    ("GB", "United Kingdom", Continent::Europe),
    ("GD", "Grenada", Continent::NorthAmerica),
    ("GE", "Georgia", Continent::Asia),
    ("GF", "French Guiana", Continent::SouthAmerica),
    ("GG", "Guernsey", Continent::Europe),
    ("GH", "Ghana", Continent::Africa),
    ("GI", "Gibraltar", Continent::Europe),
    ("GL", "Greenland", Continent::NorthAmerica),
    ("GM", "Gambia", Continent::Africa),
    ("GN", "Guinea", Continent::Africa),
    ("GP", "Guadeloupe", Continent::NorthAmerica),
    ("GQ", "Equatorial Guinea", Continent::Africa),
    ("GR", "Greece", Continent::Europe),
    ("GS", "South Georgia", Continent::SouthAmerica),
    ("GT", "Guatemala", Continent::NorthAmerica),
    ("GU", "Guam", Continent::Oceania),
    ("GW", "Guinea-Bissau", Continent::Africa),
    ("GY", "Guyana", Continent::SouthAmerica),
    ("HK", "Hong Kong", Continent::Asia),
    ("HM", "Heard and McDonald Islands", Continent::Oceania),
    ("HN", "Honduras", Continent::NorthAmerica),
    ("HR", "Croatia", Continent::Europe),
    ("HT", "Haiti", Continent::NorthAmerica),
    ("HU", "Hungary", Continent::Europe),
    ("ID", "Indonesia", Continent::Asia),
    ("IE", "Ireland", Continent::Europe),
    ("IL", "Israel", Continent::Asia),
    ("IM", "Isle of Man", Continent::Europe),
    ("IN", "India", Continent::Asia),
    ("IO", "British Indian Ocean Territory", Continent::Africa),
    ("IQ", "Iraq", Continent::Asia),
    ("IR", "Iran", Continent::Asia),
    ("IS", "Iceland", Continent::Europe),
    ("IT", "Italy", Continent::Europe),
    ("JE", "Jersey", Continent::Europe),
    ("JM", "Jamaica", Continent::NorthAmerica),
    ("JO", "Jordan", Continent::Asia),
    ("JP", "Japan", Continent::Asia),
    ("KE", "Kenya", Continent::Africa),
    ("KG", "Kyrgyzstan", Continent::Asia),
    ("KH", "Cambodia", Continent::Asia),
    ("KI", "Kiribati", Continent::Oceania),
    ("KM", "Comoros", Continent::Africa),
    ("KN", "Saint Kitts and Nevis", Continent::NorthAmerica),
    ("KP", "North Korea", Continent::Asia),
    ("KR", "South Korea", Continent::Asia),
    ("KW", "Kuwait", Continent::Asia),
    ("KY", "Cayman Islands", Continent::NorthAmerica),
    ("KZ", "Kazakhstan", Continent::Asia),
    ("LA", "Laos", Continent::Asia),
    ("LB", "Lebanon", Continent::Asia),
    ("LC", "Saint Lucia", Continent::NorthAmerica),
    ("LI", "Liechtenstein", Continent::Europe),
    ("LK", "Sri Lanka", Continent::Asia),
    ("LR", "Liberia", Continent::Africa),
    ("LS", "Lesotho", Continent::Africa),
    ("LT", "Lithuania", Continent::Europe),
    ("LU", "Luxembourg", Continent::Europe),
    ("LV", "Latvia", Continent::Europe),
    ("LY", "Libya", Continent::Africa),
    ("MA", "Morocco", Continent::Africa),
    ("MC", "Monaco", Continent::Europe),
    ("MD", "Moldova", Continent::Europe),
    ("ME", "Montenegro", Continent::Europe),
    ("MF", "Saint Martin", Continent::NorthAmerica),
    ("MG", "Madagascar", Continent::Africa),
    ("MH", "Marshall Islands", Continent::Oceania),
    ("MK", "North Macedonia", Continent::Europe),
    ("ML", "Mali", Continent::Africa),
    ("MM", "Myanmar", Continent::Asia),
    ("MN", "Mongolia", Continent::Asia),
    ("MO", "Macao", Continent::Asia),
    ("MP", "Northern Mariana Islands", Continent::Oceania),
    ("MQ", "Martinique", Continent::NorthAmerica),
    ("MR", "Mauritania", Continent::Africa),
    ("MS", "Montserrat", Continent::NorthAmerica),
    ("MT", "Malta", Continent::Europe),
    ("MU", "Mauritius", Continent::Africa),
    ("MV", "Maldives", Continent::Asia),
    ("MW", "Malawi", Continent::Africa),
    ("MX", "Mexico", Continent::NorthAmerica),
    ("MY", "Malaysia", Continent::Asia),
    ("MZ", "Mozambique", Continent::Africa),
    ("NA", "Namibia", Continent::Africa),
    ("NC", "New Caledonia", Continent::Oceania),
    ("NE", "Niger", Continent::Africa),
    ("NF", "Norfolk Island", Continent::Oceania),
    ("NG", "Nigeria", Continent::Africa),
    ("NI", "Nicaragua", Continent::NorthAmerica),
    ("NL", "Netherlands", Continent::Europe),
    ("NO", "Norway", Continent::Europe),
    ("NP", "Nepal", Continent::Asia),
    ("NR", "Nauru", Continent::Oceania),
    ("NU", "Niue", Continent::Oceania),
    ("NZ", "New Zealand", Continent::Oceania),
    ("OM", "Oman", Continent::Asia),
    ("PA", "Panama", Continent::NorthAmerica),
    ("PE", "Peru", Continent::SouthAmerica),
    ("PF", "French Polynesia", Continent::Oceania),
    ("PG", "Papua New Guinea", Continent::Oceania),
    ("PH", "Philippines", Continent::Asia),
    ("PK", "Pakistan", Continent::Asia),
    ("PL", "Poland", Continent::Europe),
    ("PM", "Saint Pierre and Miquelon", Continent::NorthAmerica),
    ("PN", "Pitcairn", Continent::Oceania),
    ("PR", "Puerto Rico", Continent::NorthAmerica),
    ("PS", "Palestine", Continent::Asia),
    ("PT", "Portugal", Continent::Europe),
    ("PW", "Palau", Continent::Oceania),
    ("PY", "Paraguay", Continent::SouthAmerica),
    ("QA", "Qatar", Continent::Asia),
    ("RE", "Réunion", Continent::Africa),
    ("RO", "Romania", Continent::Europe),
    ("RS", "Serbia", Continent::Europe),
    ("RU", "Russia", Continent::Europe),
    ("RW", "Rwanda", Continent::Africa),
    ("SA", "Saudi Arabia", Continent::Asia),
    ("SB", "Solomon Islands", Continent::Oceania),
    ("SC", "Seychelles", Continent::Africa),
    ("SD", "Sudan", Continent::Africa),
    ("SE", "Sweden", Continent::Europe),
    ("SG", "Singapore", Continent::Asia),
    ("SH", "Saint Helena", Continent::Africa),
    ("SI", "Slovenia", Continent::Europe),
    ("SJ", "Svalbard and Jan Mayen", Continent::Europe),
    ("SK", "Slovakia", Continent::Europe),
    ("SL", "Sierra Leone", Continent::Africa),
    ("SM", "San Marino", Continent::Europe),
    ("SN", "Senegal", Continent::Africa),
    ("SO", "Somalia", Continent::Africa),
    ("SR", "Suriname", Continent::SouthAmerica),
    ("SS", "South Sudan", Continent::Africa),
    ("ST", "Sao Tome and Principe", Continent::Africa),
    ("SV", "El Salvador", Continent::NorthAmerica),
    ("SX", "Sint Maarten", Continent::NorthAmerica),
    ("SY", "Syria", Continent::Asia),
    ("SZ", "Eswatini", Continent::Africa),
    ("TC", "Turks and Caicos Islands", Continent::NorthAmerica),
    ("TD", "Chad", Continent::Africa),
    ("TF", "French Southern Territories", Continent::Africa),
    ("TG", "Togo", Continent::Africa),
    ("TH", "Thailand", Continent::Asia),
    ("TJ", "Tajikistan", Continent::Asia),
    ("TK", "Tokelau", Continent::Oceania),
    ("TL", "Timor-Leste", Continent::Asia),
    ("TM", "Turkmenistan", Continent::Asia),
    ("TN", "Tunisia", Continent::Africa),
    ("TO", "Tonga", Continent::Oceania),
    ("TR", "Türkiye", Continent::Asia),
    ("TT", "Trinidad and Tobago", Continent::NorthAmerica),
    ("TV", "Tuvalu", Continent::Oceania),
    ("TW", "Taiwan", Continent::Asia),
    ("TZ", "Tanzania", Continent::Africa),
    ("UA", "Ukraine", Continent::Europe),
    ("UG", "Uganda", Continent::Africa),
    ("UM", "US Minor Outlying Islands", Continent::Oceania),
    ("US", "United States", Continent::NorthAmerica),
    ("UY", "Uruguay", Continent::SouthAmerica),
    ("UZ", "Uzbekistan", Continent::Asia),
    ("VA", "Vatican City", Continent::Europe),
    ("VC", "Saint Vincent and the Grenadines", Continent::NorthAmerica),
    ("VE", "Venezuela", Continent::SouthAmerica),
    ("VG", "British Virgin Islands", Continent::NorthAmerica),
    ("VI", "US Virgin Islands", Continent::NorthAmerica),
    ("VN", "Vietnam", Continent::Asia),
    ("VU", "Vanuatu", Continent::Oceania),
    ("WF", "Wallis and Futuna", Continent::Oceania),
    ("WS", "Samoa", Continent::Oceania),
    ("YE", "Yemen", Continent::Asia),
    ("YT", "Mayotte", Continent::Africa),
    ("ZA", "South Africa", Continent::Africa),
    ("ZM", "Zambia", Continent::Africa),
    ("ZW", "Zimbabwe", Continent::Africa),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A registry in the shape xkeyboard-config writes one, small enough to
    /// read: two countries on two continents, a layout claimed by both, one
    /// belonging to nobody, and a variant that names a country of its own.
    const FIXTURE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE xkbConfigRegistry SYSTEM "xkb.dtd">
<xkbConfigRegistry version="1.1">
  <layoutList>
    <layout>
      <configItem>
        <name>pl</name>
        <description>Polish</description>
        <countryList><iso3166Id>PL</iso3166Id></countryList>
      </configItem>
      <variantList>
        <variant>
          <configItem>
            <name>qwertz</name>
            <description>Polish (QWERTZ)</description>
          </configItem>
        </variant>
        <variant>
          <configItem>
            <name>ru_phonetic_dvorak</name>
            <description>Russian (Poland, phonetic Dvorak)</description>
            <countryList><iso3166Id>RU</iso3166Id></countryList>
          </configItem>
        </variant>
      </variantList>
    </layout>
    <layout>
      <configItem>
        <name>ara</name>
        <description>Arabic</description>
        <countryList><iso3166Id>EG</iso3166Id><iso3166Id>SA</iso3166Id></countryList>
      </configItem>
    </layout>
    <layout>
      <configItem>
        <name>brai</name>
        <description>Braille</description>
      </configItem>
    </layout>
  </layoutList>
</xkbConfigRegistry>
"#;

    fn fixture() -> Registry {
        Registry {
            layouts: parse(FIXTURE).expect("the fixture parses"),
        }
    }

    /// A country is named in the shell's language and listed where somebody
    /// reading that language would look for it: "Łotwa" after "Luksemburg"
    /// and before "Malta", not after every Z; "Åland" among the As.
    #[test]
    fn countries_are_named_and_ordered_in_the_shells_language() {
        let names = |codes: &[&str]| -> Vec<String> {
            let mut seen: std::collections::BTreeMap<crate::i18n::SortKey, String> =
                std::collections::BTreeMap::new();
            for code in codes {
                let name = country_name(code);
                seen.insert(crate::i18n::sort_key(&name), name);
            }
            seen.into_values().collect()
        };
        assert_eq!(
            names(&["ZA", "AX", "AL", "LV", "LT", "LU", "MT"]),
            [
                "Åland Islands",
                "Albania",
                "Latvia",
                "Lithuania",
                "Luxembourg",
                "Malta",
                "South Africa"
            ]
        );
        crate::i18n::set(crate::i18n::Language::Polish);
        assert_eq!(country_name("PL"), "Polska");
        assert_eq!(country_name(NOWHERE), "Bez kraju");
        assert_eq!(
            names(&["ZA", "AX", "AL", "LV", "LT", "LU", "MT", "BA", "BW"]),
            [
                "Albania",
                "Bośnia i Hercegowina",
                "Botswana",
                "Litwa",
                "Luksemburg",
                "Łotwa",
                "Malta",
                "Republika Południowej Afryki",
                "Wyspy Alandzkie"
            ]
        );
        crate::i18n::set(crate::i18n::Language::British);
    }

    /// A layout and each of its variants is one row, and the base layout comes
    /// first — which is what makes the first row of a country's column the
    /// arrangement its keyboards actually have.
    #[test]
    fn a_layout_and_its_variants_are_one_flat_list() {
        let registry = fixture();
        assert_eq!(registry.len(), 5);
        let polish: Vec<String> = registry
            .layouts_in("PL")
            .iter()
            .map(|layout| layout.key())
            .collect();
        assert_eq!(polish, ["pl", "pl (qwertz)"]);
        // The variant that named Russia is filed there and not in Poland.
        assert_eq!(
            registry
                .layouts_in("RU")
                .iter()
                .map(|layout| layout.key())
                .collect::<Vec<_>>(),
            ["pl (ru_phonetic_dvorak)"]
        );
    }

    /// A variant with no country list of its own is its layout's, which is how
    /// nearly all five hundred of them are written.
    #[test]
    fn a_variant_inherits_the_countries_its_layout_claims() {
        let registry = fixture();
        let variant = registry.find("pl", "qwertz").expect("the fixture has it");
        assert_eq!(variant.countries, ["PL"]);
        assert_eq!(variant.name, "Polish (QWERTZ)");
    }

    /// One layout, two countries, and it is under both of them — which is what
    /// a country list means.
    #[test]
    fn a_layout_several_countries_claim_is_under_each_of_them() {
        let registry = fixture();
        for code in ["EG", "SA"] {
            assert_eq!(
                registry
                    .layouts_in(code)
                    .iter()
                    .map(|layout| layout.key())
                    .collect::<Vec<_>>(),
                ["ara"],
                "{code}"
            );
        }
    }

    /// The tree leaves out the continents nothing is filed under, and puts
    /// what belongs to no country in the group that is not a continent.
    #[test]
    fn the_tree_skips_empty_continents_and_keeps_what_belongs_nowhere() {
        let registry = fixture();
        assert_eq!(
            registry.continents(),
            [
                Continent::Africa,
                Continent::Asia,
                Continent::Europe,
                Continent::Other
            ]
        );
        assert_eq!(
            registry.countries_in(Continent::Other),
            [Country {
                code: NOWHERE.to_string(),
                name: NO_COUNTRY.to_string()
            }]
        );
        assert_eq!(
            registry
                .layouts_in(NOWHERE)
                .iter()
                .map(|layout| layout.key())
                .collect::<Vec<_>>(),
            ["brai"]
        );
    }

    /// Countries are listed by name, not by code — so Egypt comes before
    /// Saudi Arabia although EG and SA are on different continents, and within
    /// one continent the order is the one somebody reads down.
    #[test]
    fn countries_are_listed_by_the_name_they_are_read_under() {
        let registry = fixture();
        assert_eq!(
            registry
                .countries_in(Continent::Asia)
                .iter()
                .map(|country| country.name.clone())
                .collect::<Vec<_>>(),
            ["Saudi Arabia"]
        );
        assert_eq!(
            registry.countries_in(Continent::Africa),
            [Country {
                code: "EG".to_string(),
                name: "Egypt".to_string()
            }]
        );
    }

    /// The field finds a layout by its own name, by its xkb name, and by the
    /// country that claims it — which is the word somebody who does not know
    /// their layout is called "Polish (QWERTZ)" will type.
    #[test]
    fn the_field_searches_names_xkb_names_and_countries() {
        let registry = fixture();
        let keys = |query: &str| {
            registry
                .search(query)
                .iter()
                .map(|layout| layout.key())
                .collect::<Vec<_>>()
        };
        assert_eq!(keys("qwertz"), ["pl (qwertz)"]);
        assert_eq!(keys("POLISH"), ["pl", "pl (qwertz)"]);
        // Three, not two: the Russian arrangement for Polish keyboards is
        // *named* after Poland, and a field that hid it because the country it
        // is filed under is Russia would be hiding the row somebody typing
        // "Poland" is most likely hunting for.
        assert_eq!(
            keys("Poland"),
            ["pl", "pl (qwertz)", "pl (ru_phonetic_dvorak)"]
        );
        assert_eq!(keys("saudi"), ["ara"]);
        // Nothing typed narrows nothing, rather than matching everything: an
        // empty field is a field nobody has searched with.
        assert!(keys("").is_empty());
        assert!(keys("   ").is_empty());
    }

    /// What a found row says about where it lives: the path somebody would
    /// have walked, or a count where there is no one path.
    #[test]
    fn a_found_row_says_where_it_lives() {
        let registry = fixture();
        let note = |layout: &str, variant: &str| {
            registry.whereabouts(registry.find(layout, variant).expect("the fixture has it"))
        };
        assert_eq!(note("pl", "qwertz"), "Europe · Poland");
        assert_eq!(note("ara", ""), "2 countries");
        assert_eq!(note("brai", ""), "No country");
    }

    /// The form both the settings file and `setxkbmap -query` write, read back
    /// into the two names xkb takes.
    #[test]
    fn a_layout_is_written_and_read_back_as_one_string() {
        assert_eq!(from_key("pl (qwertz)"), ("pl".into(), "qwertz".into()));
        assert_eq!(from_key("pl"), ("pl".into(), String::new()));
        // A hand-written file gets the benefit of the doubt: spacing, and a
        // bracket somebody forgot to close.
        assert_eq!(from_key("  pl  "), ("pl".into(), String::new()));
        assert_eq!(from_key("pl(qwertz"), ("pl".into(), "qwertz".into()));
        for layout in fixture().layouts {
            let (name, variant) = from_key(&layout.key());
            assert!(layout.is(&name, &variant), "{} round trips", layout.key());
        }
    }

    /// The doctype every registry xkeyboard-config ships is read, not refused.
    ///
    /// Its own test because the cost of getting it wrong is total and silent:
    /// a parser that refuses the declaration reads nought layouts on every
    /// machine, and a fixture without one goes on passing beside it. Which is
    /// what happened — the page shipped saying the machine had no layouts, and
    /// only a photograph of it found out.
    #[test]
    fn the_doctype_a_real_registry_opens_with_is_read() {
        assert!(FIXTURE.contains("<!DOCTYPE"), "the fixture is a real one");
        assert_eq!(parse(FIXTURE).expect("it parses").len(), 5);
    }

    /// Every ISO code appears once, and every continent name is one somebody
    /// would look for. The table is the one thing here that is written down
    /// rather than read off the machine, so it is the one thing that can be
    /// wrong without anything noticing.
    #[test]
    fn the_country_table_is_whole_and_says_each_country_once() {
        assert_eq!(COUNTRIES.len(), 249);
        let mut codes: Vec<&str> = COUNTRIES.iter().map(|(code, _, _)| *code).collect();
        codes.sort_unstable();
        let mut unique = codes.clone();
        unique.dedup();
        assert_eq!(codes, unique, "a code is listed twice");
        for (code, name, _) in COUNTRIES {
            assert_eq!(code.len(), 2, "{code} is not an alpha-2 code");
            assert!(
                code.chars().all(|letter| letter.is_ascii_uppercase()),
                "{code} is not upper case"
            );
            assert!(!name.is_empty(), "{code} has no name");
            assert_ne!(*code, NOWHERE, "{NOWHERE} stands for no country");
        }
        assert_eq!(place_of("PL"), Continent::Europe);
        assert_eq!(country_name("PL"), "Poland");
        // And a code no table knows is still reachable, under its own name.
        assert_eq!(place_of("QQ"), Continent::Other);
        assert_eq!(country_name("QQ"), "QQ");
    }

    /// The registry on the machine running the tests, where there is one.
    ///
    /// Not an assertion about which layouts it holds — that is xkeyboard-
    /// config's business and it changes with the package. What is asserted is
    /// that whatever is there parses, files itself somewhere, and is reachable
    /// from the tree the page draws. See [[tests-that-read-the-machine]].
    #[test]
    fn this_machine_s_own_registry_is_reachable_through_the_tree() {
        let registry = registry();
        if registry.is_empty() {
            // Not silently: a registry that is there and did not parse is the
            // failure this whole module can have, and it looks exactly like a
            // machine with no xkeyboard-config until somebody checks.
            assert!(
                !config_roots()
                    .iter()
                    .any(|root| root.join("rules/evdev.xml").exists()),
                "this machine has a registry and none of it was read"
            );
            return;
        }
        let mut reached = 0;
        for continent in registry.continents() {
            let countries = registry.countries_in(continent);
            assert!(!countries.is_empty(), "{continent:?} was listed empty");
            for country in countries {
                let layouts = registry.layouts_in(&country.code);
                assert!(
                    !layouts.is_empty(),
                    "{} was listed with nothing under it",
                    country.name
                );
                reached += layouts.len();
            }
        }
        // Every arrangement is under at least one country, so the walk sees
        // all of them and the ones several countries claim more than once.
        assert!(
            reached >= registry.len(),
            "{reached} rows for {} arrangements",
            registry.len()
        );
        for layout in &registry.layouts {
            let (name, variant) = from_key(&layout.key());
            assert!(registry.find(&name, &variant).is_some(), "{}", layout.key());
        }
    }
}
