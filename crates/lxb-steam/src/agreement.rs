//! The words of an agreement a game will not install without.
//!
//! Valve's client lists a game's agreements (see [`crate::webui::Eula`]) but
//! does not hold their text: its own dialog fetches each one from the address
//! in the list, with `eulaLang` and `json=1` on the end, and draws what comes
//! back. This is that fetch, done here so the shell can draw it instead.
//!
//! Measured 2026-09-23 against the store: the answer is
//! `{ title, content, eulaLang, rgLanguages }`, it needs no credentials, and a
//! language the publisher has not translated into is answered in English with
//! `eulaLang` saying so — Garry's Mod asked for in Polish comes back English,
//! TEKKEN 8 comes back Polish. `content` is plain text with `\r\n` line ends,
//! sometimes with Steam's BBCode in it (`[b]`, `[i]`, `[url]` in Black Mesa's),
//! and sometimes no more than a link to the publisher's site (Grand Theft Auto
//! V's is a single `[url]`).

use crate::webui::Eula;
use std::time::Duration;

/// One agreement, with its words where they could be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agreement {
    /// Which agreement it is, and what accepting it records.
    pub eula: Eula,
    /// Its own name for itself — "Facepunch Terms of Service" — or empty
    /// where it gave none.
    pub title: String,
    /// What it says, as plain text: paragraphs separated by a blank line, a
    /// list item on a line of its own. `None` when it could not be read,
    /// which is an agreement nobody may be asked to accept.
    pub text: Option<String>,
}

/// How long one agreement may take to arrive. The press is standing while it
/// does — the row says it is installing — so this is patience for a slow
/// connection, not for a server that has gone.
const PATIENCE: Duration = Duration::from_secs(15);

/// The most an answer is read to. The longest seen is 59 KB; this is room for
/// a publisher that writes a great deal more and none for a server that
/// sends something that is not an agreement at all.
const LARGEST: u64 = 2 * 1024 * 1024;

/// Read each agreement's words, in `language` where the publisher wrote one.
///
/// `language` is Steam's own name for it — `english`, `polish`, `schinese` —
/// which is what `eulaLang` takes. One that cannot be read is carried with no
/// text rather than dropped, so a game with two agreements is never presented
/// as having one.
pub fn read_all(eulas: Vec<Eula>, language: &str) -> Vec<Agreement> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(PATIENCE))
        .user_agent(concat!("LineXinBar/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    eulas
        .into_iter()
        .map(|eula| match read(&agent, &eula, language) {
            Ok((title, text)) => Agreement {
                eula,
                title,
                text: Some(text),
            },
            Err(why) => {
                tracing::warn!(id = %eula.id, url = %eula.url, %why, "an agreement could not be read");
                Agreement {
                    eula,
                    title: String::new(),
                    text: None,
                }
            }
        })
        .collect()
}

fn read(agent: &ureq::Agent, eula: &Eula, language: &str) -> Result<(String, String), String> {
    let url = address(&eula.url, language).ok_or("not an address this shell will fetch")?;
    let body = agent
        .get(&url)
        .call()
        .map_err(|error| error.to_string())?
        .into_body()
        .with_config()
        .limit(LARGEST)
        .read_to_vec()
        .map_err(|error| error.to_string())?;
    let said: serde_json::Value =
        serde_json::from_slice(&body).map_err(|error| error.to_string())?;
    let content = said
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or("the answer had no text in it")?;
    let text = plain(content);
    if text.is_empty() {
        return Err("the text was empty".to_string());
    }
    let title = said
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(|title| plain(title).replace('\n', " "))
        .unwrap_or_default();
    Ok((title, text))
}

/// Where to ask for one agreement's words.
///
/// Upgraded to HTTPS the way Valve's own dialog upgrades it — the list carries
/// `http://` addresses — and refused outright when it is anything but a web
/// address: this is a string out of another program's memory, and it is not
/// going to be handed to anything that would open a file.
fn address(url: &str, language: &str) -> Option<String> {
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    if rest.is_empty() || rest.starts_with('/') || rest.contains(char::is_whitespace) {
        return None;
    }
    let joiner = if rest.contains('?') { '&' } else { '?' };
    let language: String = language
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    Some(format!("https://{rest}{joiner}eulaLang={language}&json=1"))
}

/// An agreement's text as plain text.
///
/// Steam's BBCode taken out, keeping what it said: a link is its words, or its
/// address where it has no words; a list item begins a line with a bullet; a
/// heading or a rule is a line of its own; a picture is dropped. A bracket that
/// is not one of Steam's tags is somebody's writing and is left alone — PUBG's
/// agreement says "[including …]" in the middle of a sentence.
///
/// Then tidied the way a reader wants it: line ends made one kind, each line's
/// trailing space taken off, and never more than one blank line in a row.
pub fn plain(content: &str) -> String {
    let content = content.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(content.len());
    // Where the words of an open `[url=…]` began in `out`, and its address.
    let mut link: Option<(usize, String)> = None;
    let mut rest = content.as_str();
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after
            .find(']')
            .filter(|close| !after[..*close].contains('\n'))
        else {
            out.push('[');
            rest = after;
            continue;
        };
        let inside = &after[..close];
        let (ending, body) = match inside.strip_prefix('/') {
            Some(body) => (true, body),
            None => (false, inside),
        };
        let name_end = body.find(['=', ' ']).unwrap_or(body.len());
        let name = body[..name_end].to_ascii_lowercase();
        let argument = body[name_end..]
            .strip_prefix('=')
            .map(|value| value.trim_matches(['"', '\'']).to_string());
        rest = &after[close + 1..];
        match name.as_str() {
            "url" if !ending => link = Some((out.len(), argument.unwrap_or_default())),
            "url" => {
                if let Some((from, address)) = link.take() {
                    if out[from..].trim().is_empty() && !address.is_empty() {
                        out.truncate(from);
                        out.push_str(&address);
                    }
                }
            }
            // A picture has nothing to say in text. What is between its tags
            // is an address, and it goes too.
            "img" if !ending => match rest.to_ascii_lowercase().find("[/img]") {
                Some(end) => rest = &rest[end + "[/img]".len()..],
                None => rest = "",
            },
            "*" => {
                line_break(&mut out);
                out.push_str("• ");
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "hr" | "list" | "olist" | "ul" | "ol"
            | "quote" | "code" | "table" | "tr" => line_break(&mut out),
            "td" | "th" if !ending => out.push(' '),
            "b" | "i" | "u" | "s" | "strike" | "spoiler" | "noparse" | "img" | "td" | "th"
            | "emoticon" | "p" => {}
            // Not one of Steam's: somebody's own square brackets.
            _ => {
                out.push('[');
                out.push_str(inside);
                out.push(']');
            }
        }
    }
    out.push_str(rest);
    tidy(&out)
}

/// Start a new line, unless one has just been started.
fn line_break(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn tidy(text: &str) -> String {
    let mut tidied = String::with_capacity(text.len());
    let mut blank = 0;
    for line in text.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            blank += 1;
            continue;
        }
        if !tidied.is_empty() {
            tidied.push_str(if blank > 0 { "\n\n" } else { "\n" });
        }
        blank = 0;
        tidied.push_str(line);
    }
    tidied
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The address Valve's dialog asks, with the edition of HTTP it asks it
    /// over — and nothing that is not a web address at all.
    #[test]
    fn the_address_is_the_one_the_client_asks() {
        assert_eq!(
            address("http://store.steampowered.com/eula/4000_eula_0", "polish").as_deref(),
            Some("https://store.steampowered.com/eula/4000_eula_0?eulaLang=polish&json=1")
        );
        assert_eq!(
            address("https://example.com/terms?x=1", "english").as_deref(),
            Some("https://example.com/terms?x=1&eulaLang=english&json=1")
        );
        // The language is a name, and nothing else gets through with it.
        assert_eq!(
            address("https://example.com/t", "english&json=0").as_deref(),
            Some("https://example.com/t?eulaLang=englishjson0&json=1")
        );
        for refused in [
            "file:///etc/passwd",
            "",
            "https://",
            "https:///x",
            "ftp://x",
            "https://a b",
        ] {
            assert_eq!(address(refused, "english"), None, "{refused}");
        }
    }

    /// Plain text stays as it was written, apart from its line ends and its
    /// run of blank lines.
    #[test]
    fn plain_text_is_left_as_written() {
        assert_eq!(
            plain("Terms of Service\r\n\r\nLast updated on 15 July 2025\r\n\r\n\r\n\r\nA quick summary:   \r\n* Rules."),
            "Terms of Service\n\nLast updated on 15 July 2025\n\nA quick summary:\n* Rules."
        );
    }

    /// Black Mesa's markup, and the things the rest of Steam's BBCode does.
    #[test]
    fn steams_markup_is_read_for_what_it_says() {
        assert_eq!(
            plain("[b]1. Definitions[/b]\r\n\"[b]Account[/b]\" means [i]any[/i] account."),
            "1. Definitions\n\"Account\" means any account."
        );
        // A link is its words, or its address where it has none.
        assert_eq!(
            plain("[url]https://www.rockstargames.com/legal?country=pl[/url]"),
            "https://www.rockstargames.com/legal?country=pl"
        );
        assert_eq!(
            plain("See [url=https://example.com/privacy]our privacy policy[/url]."),
            "See our privacy policy."
        );
        assert_eq!(
            plain("See [URL=\"https://example.com\"][/URL]."),
            "See https://example.com."
        );
        // Lists, headings and pictures.
        assert_eq!(
            plain("[h1]Rules[/h1][list][*]One[*]Two[/list][img]https://x/y.png[/img]End"),
            "Rules\n• One\n• Two\nEnd"
        );
    }

    /// Square brackets that are not Steam's tags are somebody's writing.
    #[test]
    fn a_bracket_that_is_not_a_tag_is_writing() {
        assert_eq!(
            plain("any claim [including negligence] and [1] and a lone [ bracket"),
            "any claim [including negligence] and [1] and a lone [ bracket"
        );
        assert_eq!(plain("[unclosed\nline] here"), "[unclosed\nline] here");
    }
}
