# Shell languages

[Documentation](index.md) · [Project home](../README.md)

Settings → Language offers ten languages — **Deutsch**, **English (UK)**,
**English (US)**, **Español**, **Français**, **Polski**, **Português (Brasil)**,
**Русский**, **हिन्दी** and **简体中文** — every language the shell has a catalog
for, which is also every language the login screen speaks. A choice is the
language of the *machine*, not a private setting of the shell's: the shell
changes its own text on the spot, on every display, and then makes the system
speak the same language. It is saved in `$XDG_CONFIG_HOME/lxb/shell.toml`
(normally `~/.config/lxb/shell.toml`):

```toml
language = "pl"
```

The accepted values are `de`, `en-GB`, `en-US`, `es`, `fr`, `hi`, `pl`,
`pt-BR`, `ru` and `zh-CN`. A missing or unrecognized value, or `system`, means
nothing has been chosen: the shell follows the session's locale — the first
nonempty of `LC_ALL`, `LC_MESSAGES`, `LANG` at startup, read by its language
(any `de_*` is German, any `pt_*` is the Brazilian catalog, any `zh_*` the
Simplified one), with one region looked at: `en_US` is American English and
every other English, and every language the shell has no catalog for, C and
POSIX included, is British English — and changes nothing on the machine. There
is no "System default" row, because every row on the page sets the system's
language, and an entry meaning "whatever the system has" would contradict the
list it sits in.

The list reads in the order the languages' own names sort in, alphabet by
alphabet: the Latin names, then the Cyrillic one, the Devanagari one, the Han
one. Two of those names are in alphabets Roboto has not got, which is why the
Devanagari and Han faces travel with the shell whatever language it is in —
see "Faces" below.

A bare `en` is what every file written before the two Englishes were told apart
says, and is read as `en-GB`: that is the catalog those shells drew their words
from, so such a file goes on saying what it always said. Pressing either row
rewrites it to a tag that names a region.

### The two Englishes

They differ in two things and in nothing else:

* **The order of a date.** `16 September 2026` and `16/09/2026` against
  `September 16, 2026` and `09/16/2026`; the start screen's corner reads `6/9`
  against `9/6`. Every date the user reads goes through the catalog, so this is
  one line per form and not a rule spread through the code.
* **A handful of spellings** — colour, and whatever joins it.

So `en-US.ftl` is an **overlay**, not a second catalog: it holds only those
messages, and everything else is answered out of `en-GB.ftl`, which is the
English the shell is written in and the language every other one falls back to.
A message copied across unchanged is a message that has to be edited twice from
the day it is copied, and `i18n::tests::the_american_catalog_is_only_what_america_writes_differently`
fails a catalog that does it.

Which clock a time of day is written on is deliberately *not* one of the two:
it is its own setting, under Settings → System → Clock, and it merely falls
back to the language. See "The clock" below.

## What choosing a language changes

`crates/lxb-desktop/src/locale.rs` carries a choice three ways:

1. **The system locale**, through `systemd-localed` (`org.freedesktop.locale1`
   on the system bus). `SetLocale(["LANG=<locale>"], interactive)` writes
   `/etc/locale.conf`, tells the service manager, and on a distribution built
   round `locale-gen` generates a locale that is not yet installed. Only
   `LANG` is sent; localed merges it with the file, so an `LC_TIME` somebody
   set apart from their language stays as it was. polkit's question, if the
   policy asks one (`org.freedesktop.locale1.set-locale` is `auth_admin_keep`
   by default), is answered through the shell's own agent — the same password
   panel Users and Network raise. A locale the file already names is answered
   with success and no question.
2. **The environment of every program the shell opens.** The shell is the
   session's launcher, so it rewrites its own `LANG`, and `LC_ALL`,
   `LC_MESSAGES` and `LANGUAGE` where the session already had them (each
   outranks `LANG` when present). Nothing already running changes — a program
   reads its locale once at its start — so the row says "from now on" in
   effect. At startup the same is done without a bus call, so a shell set to
   Polish on a session that came up English still opens Polish applications.
3. **The session bus's activation environment and the systemd user manager's
   environment**, best effort, so a program D-Bus starts on somebody's behalf
   follows too.
4. **The machine's own environment files** — `/etc/environment`, which PAM
   reads into every login session, and `/etc/environment.d/*.conf` — where one
   of them names another language. Those files are root's, so this half runs
   as a separate short-lived process that polkit starts; see below.

The locale a language gets is the one the session started in when that already
speaks the language (`en_AU.UTF-8` stays Australian when English (UK) is
chosen, because every English but the American one is read out of the British
catalog; `fr_CA.UTF-8` stays Canadian when Français is chosen; `pt_PT.UTF-8`
stays European when Português (Brasil) is, and `zh_TW.UTF-8` stays Taiwanese
when 简体中文 is, because each of those is the whole language's catalog),
otherwise the installer's first offer: `de_DE.UTF-8`, `en_GB.UTF-8`,
`en_US.UTF-8`, `es_ES.UTF-8`, `fr_FR.UTF-8`, `hi_IN.UTF-8`, `pl_PL.UTF-8`,
`pt_BR.UTF-8`, `ru_RU.UTF-8`, `zh_CN.UTF-8`. That is also what carries the date
format to everything the shell did not draw: `LC_TIME` is merged out of
`LANG`, so a browser, a spreadsheet and `date` in a terminal write the American
order under `en_US.UTF-8` and the British one under `en_GB.UTF-8` without being
told twice.

`LANGUAGE`, where a file or the environment has one, takes the *locale* name —
`en_GB`, not `en-GB`. glibc splits a locale at the underscore and would read a
hyphenated tag as one long language name it has no catalog for.
A locale glibc cannot load is never exported: the note under the row says the
locale is not installed, and applications keep the language they had, rather
than every one of them opening in the C locale and warning about it.

## What would outrank it, and what is done about it

`SetLocale` writes `LANG` into `/etc/locale.conf`, and that file is read once,
by the service manager, at boot. Every login session then has PAM read
`/etc/environment` over the top of it — and glibc reads `LC_ALL`, `LC_MESSAGES`
and `LANGUAGE` before it looks at `LANG` at all. A machine whose
`/etc/environment` (or `/etc/environment.d/*.conf`) says `LC_ALL=en_GB.UTF-8`
would therefore go on speaking English to everything this shell did not start —
**the login screen above all**, but also a service, a session on another seat
and a TTY login — however carefully the system locale was set.

So the shell changes those files too, from the same press. Choosing a language
looks at them first:

* **Nothing in them names another language** — the ordinary machine. The locale
  service is asked from the session, exactly as before: one password question,
  polkit's own, against `org.freedesktop.locale1.set-locale`.
* **Something does.** The whole job goes to one short-lived root process
  instead — `lxb-desktop --apply-language <locale>`, which polkit starts for
  the action `org.linexinbar.locale.apply`. It sets the system locale through
  the locale service (a root process needs no polkit permission of its own to
  ask localed) and then changes the files. Still **one** password question, and
  the panel says "Change the system language" because polkit reads the
  installed action's own description.

What that root half may do is deliberately small, and that is what makes the
action safe to hold: it takes one argument, checks it is the name of a locale
glibc can actually load, and then only ever **changes the value** of `LANG`,
`LC_ALL`, `LC_MESSAGES` or `LANGUAGE` where a file already has one, to that
locale. It cannot add a line, remove one, or write any other variable, so the
worst that can come out of it is the thing the action is for. Everything else
in the file — the other variables, the comments, the spacing, the quotes — is
written back byte for byte, through a temporary file and a rename so that a
machine losing power mid-write has one whole version or the other. A *country*
somebody chose is left alone where the shell has no catalog for it: `en_AU`
stays Australian when English (UK) is chosen. `en_GB` and `en_US` are the
exception, because since the split those two really are two of this shell's
languages and the difference between them is the thing the row is for.

The session's own `~/.config/environment.d/*.conf` is this account's, so it is
changed with no question at all.

The policy file is `packaging/files/org.linexinbar.locale.policy.in`, installed
to `/usr/share/polkit-1/actions/`, bound to the shell's installed path and to
the `--apply-language` flag by name — `packaging/check.sh` fails a package
whose action is missing, unbound, or still carrying its placeholder. A machine
with no `pkexec` falls back to asking the locale service from the session, and
the row then says the language reaches the shell and what it opens.

The row says the part a person can act on and nothing else: "A restart may be
needed to apply it everywhere" once it is done — the login screen and every
service started before the press read their language once and are still holding
the old one — or "Choose it again to apply it everywhere" when nobody
authorized the change to the machine. Which file named which variable goes to
the journal at `warn`, at startup as well as on a press, which is where to look.

A related trap, and not one the shell can see either: a `.desktop` file under
`~/.local/share/applications` **shadows** the one the package installed, by
name, whatever either says. A leftover copy of an application's entry there
keeps that application's *row on the bar* in the language it was written in,
even while the application itself comes up translated. `ls
~/.local/share/applications` is where to look.

The note under the language in force says where the system stands on it:
"In use by the shell and the system"; "Changing the system language…" while
localed is being asked; "In use by the shell and what it opens; the system's
own setting was not changed. *why*" when polkit or localed refused, in the
daemon's words; "In use by the shell only. *why*" when the locale is not
installed; a sentence about there being no locale service on a machine without
one; "In use by the shell and the system. A restart may be needed to apply it
everywhere" after a press that reached the machine; and "In use by the shell
and what it opens. Choose it again to apply it everywhere" after one that did
not. The last two are only ever reached from a press: a session that has just
started *is* the restart, so a shell that opens in Polish says simply that the
shell and the system are speaking it. Under the other row is what a press would
do.

The preference controls shell labels, dates, localized `.desktop` names,
comments and search keywords, and — through the above — the language of
applications opened from then on. It does not change keyboard layouts (those
are Settings → Input) or account settings. Names provided by users, devices,
games, and files retain their original spelling. Messages and diagnostic
details supplied by external programs remain in the language those programs
use, and so does what localed says when it refuses. Already delivered
notifications retain their text.

## The clock

Settings → System → Clock says which of the two clocks every time of day in the
session is written on:

```toml
clock = "12-hour"
```

The accepted values are `24-hour` and `12-hour`. A missing or unrecognized
value, or `language`, means nothing has been chosen and the **language**
answers: English (US) and हिन्दी write `8:10 PM`, which is the clock America
and India both read, and every other language the shell speaks writes `20:10`.
Every file written before this key existed says nothing, so nothing changes
for anybody who has not chosen one of those two.

The corner's AM and PM are written in Latin capitals in every language,
Chinese included, where 下午 would be the word: the corner draws its clock from
a closed set of characters cut from the face (see item 6 under "Adding or
editing translations"), and a language's `clock-am` and `clock-pm` have to be
made of them.

It is a setting of its own rather than a fact about the language because the
two questions really are separate — somebody who reads English as America
writes it may still want the twenty-four hour clock a console shows — and it is
under System rather than under Language because what it changes is how this
machine *writes*, not what language it writes in.

The page has two rows and neither of them is Off: what is being chosen is which
of two forms a time takes, and a switch reading "12-hour clock: Off" would
leave somebody to work out that off means twenty-four. Each row carries **this
minute, written its own way**, and the row in force is marked whether or not
anybody has chosen it — so a page nobody has pressed still says which clock the
shell is already on.

One answer for the whole session and for everything in it: the start screen's
corner, the guide's header, the night light's schedule on the Settings column,
the trash, the updates history and an achievement's unlock time all read it
through `i18n::time_of_day`. A time written into a *file name* or a *file
format* does not — the trash's `DeletionDate` is RFC 3339 and a screenshot is
called what sorts, and neither is somebody's setting to change.

It reaches beyond the shell the way the button hints do, through `shell.toml`:
the login screen reads the account's copy, and anything built on lxb-toolkit
reads it for itself.

## The rest of the family

The applications that come with this shell — Pictures, Videos, the Software
Hub — and the file question the toolkit draws for them speak from catalogs of
the same shape, chosen by the session's locale, which is the one this shell
exports. Their translator workflow is
[lxb-toolkit's](../../lxb-toolkit/docs/localization.md), and the login screen
(CEDM) keeps a form of its own for the reason given there. A language added
here is added there one repository at a time; nothing has to move together.

## Adding or editing translations

The catalogs are embedded from `crates/lxb-desktop/locales/`: `en-GB.ftl`,
the overlay `en-US.ftl`, and the whole translations `de.ftl`, `es.ftl`,
`fr.ftl`, `hi.ftl`, `pl.ftl`, `pt-BR.ftl`, `ru.ftl` and `zh-CN.ftl`. No
catalog files need to be installed alongside the executable. The language
service in `src/i18n.rs` parses each catalog once and falls back to `en-GB.ftl`
for missing messages or formatting failures — which is what makes `en-US.ftl`
an overlay rather than a copy.

1. Add a readable message ID and the complete English sentence to `en-GB.ftl`.
   IDs name what the message is for (`trash-items-still-there`,
   `steam-free-on-largest-library`), never a hash of the text. The ID is a
   name, fixed by the code that asks for it, and does not change when a
   catalog spells the sentence differently — `shell-the-colour-of-being-chosen`
   is the name of that row whatever the row says.
   **Add nothing to `en-US.ftl`** unless America really writes it differently.
2. Add the corresponding translation to each of the eight whole catalogs,
   preserving named arguments. Pass numeric counts as *numbers* —
   `"count" => items`, never `items.to_string()` — or Fluent cannot select
   `one`/`few`/`many` and Polish and Russian silently get the `other` branch
   every time.
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
6. The corner's clock is drawn from a closed set of characters cut from the
   face — digits, `:`, `/`, `.`, `%`, the space and the `A`, `M`, `P` that AM
   and PM are written with — and a run with any other character is not drawn at
   all, so a language's `clock-corner`, `clock-24-hour`, `clock-12-hour`,
   `clock-am` and `clock-pm` use only those; `PAD2($n)` writes a day or month
   with two digits where the language wants `16.09`. A row whose text the shell computes (a shelf's count, a
   listing's) is said again on a language change by whoever computed it — the
   media worker is asked to re-publish its shelves; a folder row carries
   `comment_message` beside `title_message` so both its lines are relabelled.
7. Names the shell orders for the user go through `i18n::sort_key`, which
   files a Polish letter after its plain one and an accent from elsewhere
   under its plain letter; byte order would put "Łotwa" and "Åland" after Z.
   Cyrillic, Devanagari and Han names stand in their code points, which is
   the order their own dictionaries keep or near it — ё is filed with е —
   except that a Han list comes out in radical order rather than by pinyin,
   which a reader of Chinese will notice and which nothing here yet sorts by.
   The keyboard-layout column's countries (`country-*`, by ISO 3166 code) are
   the one list that uses it so far.
8. Include new cached rows in the language-refresh path. Retain IDs, search
   queries, sorting, and selection. Shell folder constructors record the message
   key separately from the displayed title; update that key if repurposing a row.
9. Run `cargo fmt -p lxb-desktop`, `cargo check -p lxb-desktop`, and
   `cargo test -p lxb-desktop`. Check the languages visually at the intended
   display size, especially dialogs and longer Settings descriptions. A dialog
   field's label keeps 42 % of the line and takes more only when its answer
   leaves the room, so a long translated label is cut only against a long
   answer.

Polish and Russian need `one`, `few`, and `many` plural forms, plus `other` for
fractional values. German, Spanish, French, Hindi and Portuguese need two, and
they disagree about nought: CLDR puts 0 in `one` for French, Hindi and
Brazilian Portuguese (*0 fichier*, *0 फ़ाइल*, *0 arquivo*) and in `other` for
German and Spanish (*0 Dateien*, *0 archivos*). Chinese has one form for every
number and writes no selector at all. None of those rules is written anywhere
in this repository — all are Fluent's, which is the whole reason a count is
passed as a **number**; a count passed as a string matches nothing and every
language silently takes `other`, which for French is wrong at exactly one
value and therefore never noticed.

The decimal mark follows the language and not the country: `i18n::decimal`
writes a comma for German, Spanish, French, Polish, Portuguese and Russian, and
a full stop for the two Englishes, Hindi and Chinese. The catalog tests
exercise key and argument parity, message formatting, locale fallback, and
counts including 0, 1, 2, 5, 12, 22, and 112. Dialog notes wrap using the
bundled faces' measured glyphs.

### Faces

Roboto carries Latin, Greek and Cyrillic, which is eight of the ten. Hindi is
written in Devanagari and Chinese in Han, so two Noto faces travel with the
shell beside it, regular and bold, under `font/NotoSansDevanagariUI/` and
`font/NotoSansCJKsc/` — the first the whole block, the second a subset cut by
`scripts/subset-han-face.py` to the 6,763 characters of GB 2312 and the
punctuation Chinese is written with, because the whole face is twenty
megabytes a weight. They are read after Roboto and before the machine's own
fonts, so a Devanagari or a Han word is shaped in the face the layout was
measured against. `gpu::tests::every_catalog_word_is_drawn_by_a_bundled_face`
shapes every line of every catalog through the bundled faces alone and fails
on the first character none of them has; a sentence that fails it is reworded,
not a face regrown — it is the test that found the Polish catalog's `→`, which
Roboto has not got. A name from elsewhere in a character outside the subset
draws from the machine's fonts, as a Japanese title always has. The same four
files are transcribed into lxb-toolkit and held byte for byte by its sync
check.

To add another language, register its stable tag and native name in `Language`,
embed its catalog, and extend locale resolution and catalog validation; if it
is written in a script none of the bundled faces carries, bundle one. To add
another *regional* English — `en-AU`, say — the work is smaller: a file beside
`en-US.ftl` holding only what that country writes differently, one variant on
`Language`, and one arm in `i18n::supported`, which is the one place a region
is looked at. The current catalogs and shaping are intended for left-to-right
languages; adding a right-to-left language also requires bidi and layout
review.
