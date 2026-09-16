# Shell languages

Settings → Language offers **System default**, **English**, and **Polski**.
A selection takes effect immediately on every display and is saved in
`$XDG_CONFIG_HOME/lxb/shell.toml` (normally `~/.config/lxb/shell.toml`):

```toml
language = "pl"
```

The accepted values are `en`, `pl`, and `system`. Missing or unrecognized values
use English, preserving the behavior of existing installations. System default
reads the first nonempty `LC_ALL`, `LC_MESSAGES`, or `LANG` at startup. Polish
regional locales use Polish; other unsupported languages and C/POSIX use English.
Changing the system locale requires restarting the shell.

This preference controls shell labels, dates, and localized `.desktop` names,
comments, and search keywords. It does not change keyboard layouts, environment
variables, account settings, or the language of launched applications. Names
provided by users, devices, games, and files retain their original spelling.
Messages and diagnostic details supplied by external programs remain in the
language those programs use. Already delivered notifications retain their text.

## Adding or editing translations

The catalogs are embedded in `crates/lxb-desktop/locales/en.ftl` and `pl.ftl`.
No catalog files need to be installed alongside the executable. The language
service in `src/i18n.rs` parses each catalog once and falls back to English for
missing messages or formatting failures.

1. Add a readable message ID and the complete English sentence to `en.ftl`.
   IDs name what the message is for (`trash-items-still-there`,
   `steam-free-on-largest-library`), never a hash of the text.
2. Add the corresponding Polish translation to `pl.ftl`, preserving named
   arguments. Pass numeric counts as *numbers* — `"count" => items`, never
   `items.to_string()` — or Fluent cannot select `one`/`few`/`many` and Polish
   silently gets the `other` branch every time.
3. Use `crate::i18n::text("message-id")` for static labels, or
   `crate::message!("message-id", "count" => count)` for variable messages.
   Translate whole sentences instead of joining translated word fragments.
   Where a sentence has to name a *kind* of thing — "3 audio files", "the
   things you are copying" — the code passes a fixed word (`"what" => "audio"`,
   `"kind" => "move"`) and each catalog selects on it, so every language
   declines the noun and inflects the verb itself. See `search-matched`,
   `clash-many` and `sound-no-device` for the pattern.
4. A panel may put a name on a `Heading` line of its own, with a `Note` above
   and below it; each of those Notes is translated as the line it is, and reads
   as one on its own. A Note that is one sentence is one message: Notes wrap.
5. Keep protocol keys, config values, commands, paths, and navigation identities
   independent of translated labels. Folder identities must remain stable.
   `i18n::builtin` is only for fixed shell-owned metadata tables — including
   the words a sibling crate hands over, such as lxb-updates' provider names,
   lxb-protocol's corner names and lxb-steam's presence states, which the
   `integration-*` messages translate by their English value; never pass user
   text or external names to it. A panic message is not shell text.
6. Names the shell orders for the user go through `i18n::sort_key`, which
   files a Polish letter after its plain one and an accent from elsewhere
   under its plain letter; byte order would put "Łotwa" and "Åland" after Z.
   The keyboard-layout column's countries (`country-*`, by ISO 3166 code) are
   the one list that uses it so far.
7. Include new cached rows in the language-refresh path. Retain IDs, search
   queries, sorting, and selection. Shell folder constructors record the message
   key separately from the displayed title; update that key if repurposing a row.
8. Run `cargo fmt -p lxb-desktop`, `cargo check -p lxb-desktop`, and
   `cargo test -p lxb-desktop`. Check both languages visually at the intended
   display size, especially dialogs and longer Settings descriptions. A dialog
   field's label keeps 42 % of the line and takes more only when its answer
   leaves the room, so a long translated label is cut only against a long
   answer.

Polish needs `one`, `few`, and `many` plural forms, plus `other` for fractional
values. The catalog tests exercise key and argument parity, message formatting,
locale fallback, and counts including 0, 1, 2, 5, 12, 22, and 112. Dialog notes
wrap using the bundled font's measured glyphs. Font tests include every Polish
accented letter.

To add another language, register its stable tag and native name in `Language`,
embed its catalog, and extend locale resolution and catalog validation. The
current catalogs and shaping are intended for the two shipped left-to-right
languages; adding a right-to-left language also requires bidi and layout review.
