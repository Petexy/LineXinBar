# The website

[Documentation](index.md) · [Project home](../README.md)

- [What it is made of](#what-it-is-made-of)
- [Publishing it](#publishing-it)
- [Changing it](#changing-it)
- [The glyphs](#the-glyphs)
- [The sounds](#the-sounds)
- [Languages](#languages)
- [Search engines and language models](#search-engines-and-language-models)
- [Trying it locally](#trying-it-locally)

The project's website lives in [`site/`](../site) and is published to GitHub
Pages. It is not a template with the shell's colours on it: its start screen is
the shell's bar, laid out from the constants in `crates/lxb-desktop/src/ui.rs`
and gliding on the same spring, over a WebGL port of the shell's own wallpaper
shader, with the shell's glyphs, sounds, palettes and guide. Every other page is
a column of that bar stepped into.

## What it is made of

```
site/
  index.html            the start screen: the bar, and the site map
  overview.html         LineXinBar: what it is, why, screenshots, status
  install.html          trying it, packages, the session, requirements
  games.html            Steam, Epic Games, RetroArch, discs, PS3, trophies
  media.html            applications, search, music, videos, pictures, files
  guide.html            the guide overlay and what is on it
  displays.html         several displays, modes, HDR, night light, OLED
  settings.html         accent, theme, wallpaper, language, network, power…
  controls.html         pads, the guide button, keyboard, touch, bindings
  desktop.html          screenshots, sharing, file dialogs, prompts
  family.html           the login screen, the toolkit and the four apps
  develop.html          documentation, architecture, translations, bugs
  404.html              what GitHub Pages serves for a missing address
  sitemap.xml           every page in every language, for search engines
  llms.txt              the site in a page of Markdown, for language models
  llms-full.txt         the English text of every page, as Markdown
  de/ en-us/ es/ fr/    the same pages in the shell's other languages,
  pl/ pt-br/ ru/ hi/      made by tools/translate.py — never edited by hand
  zh-cn/
  assets/
    css/lxb.css         the palette as tokens, glass, rows, the guide
    js/wallpaper.js     the wallpaper, glass and bloom, in WebGL2
    js/shell.js         palette, clock, input, sounds, legend, guide, splash
    js/xmb.js           the start screen's bar and its search
    js/page.js          a column page: its rows, its panes, Back
    js/i18n.js          the scripts' own words, in every language
    js/sounds.js        the shell's interface clips, embedded (see below)
    glyphs/lg, sm       the glyphs, rendered in their material (see below)
    fonts/              the shell's faces, cut by alphabet (see below)
    sounds/             the start screen's music
    img/                screenshots of the shell and of its siblings
  tools/
    render-glyphs.py    makes assets/glyphs from the shell's own SVGs
    glyphs/             the one glyph the site needs that the shell has not got
    embed-sounds.py     makes js/sounds.js and the music from the shell's clips
    translate.py        makes every language's pages from the English ones
    seo.py              what search engines and language models read (below)
    i18n/<language>/    what each language says for each piece of text
    subset-fonts.py     makes assets/fonts from the shell's own font files
```

**`wallpaper.js` is a transcription, not an imitation.** `wallpaper()`,
`water()`, `silk()` and the sparkles are ported line for line from
`crates/lxb-desktop/src/shaders.wgsl` into GLSL ES 3.00 (`select(a, b, c)` is
`c ? b : a`; `bitcast<u32>(i32(x))` is `uint(int(x))`), and the palettes are
`theme.rs`'s twelve, converted to linear light the way `Color::rgb` converts
them. A change to the wallpaper in the shell should be carried here the same
way, by diffing the two functions. On the start screen the scene is drawn into
a texture first so the category tiles and the chosen row's disc can be drawn as
the glass branch of `fs_quad` draws them, bending the scene at their rims.

The wallpaper's clock is carried from page to page in `sessionStorage`, so
moving between pages is one continuous picture — the site's version of the
hand-over record the login screen gives the shell. Pages draw it at thirty
frames a second and the start screen at the display's rate; a machine that
cannot keep up is drawn at a lower resolution rather than a lower rate, and
`prefers-reduced-motion` gets a still frame. Without WebGL2 every page falls
back to the theme's own gradient and the bar's tiles are drawn by CSS.

**The guide is the shell's `build_guide`.** Opening it slides a column in from
the left and flies the page back into a card beside it, laid out by
`lxb_protocol::overview` — the page's elements scaled into the card, the
wallpaper drawn again inside it as a miniature, and the wallpaper round it
softened. The column is one pane of the sidebar's own glass (depth 15, frost
0.46, gloss 0.66, a faint bow across its face) over its two pools of accent
light, drawn by `wallpaper.js`; the chips on it are the page's, in the shell's
reference pixels, and the light on the chosen one glides between them on the
shell's spring. So the page can fly in one piece, `shell.js` gathers everything
but the wallpaper and its own furniture into one `.screen` element as the page
loads.

**Input is the shell's five acts.** A direction, Select, Back, Options and
Guide arrive from the keyboard, from any pad through the Gamepad API (read by
position, so the bottom face button selects on every layout), or from a
pointer, and go to the guide while it is open and to the page's screen
otherwise. The legend in the corner names whichever control was used last. On
the start screen every letter begins a search of the whole bar, so there — as
in the shell — `Esc` opens the guide; everywhere else it is `G`, `Home` or a
pad's guide button, and the button at the top left.

**What a visitor chooses** — the accent, Water or Silk, particles, the
interface sounds, the music and their volume — is kept in their browser's
`localStorage` and nowhere else.

## Publishing it

`.github/workflows/pages.yml` publishes `site/` (without `site/tools/`) on every
push to `master` or `main` that touches it, and can be run by hand from the
Actions tab. It has to be switched on once, under the repository's
**Settings > Pages > Build and deployment > Source: GitHub Actions**. Nothing
is built: the site is plain HTML, CSS and JavaScript, with no dependencies and
nothing fetched from anywhere else.

## Changing it

- **A page** is plain HTML. Its rows are the `.rows` list, each an `<a
  class="row" href="#section" data-lxb-row>`, and its panes are the
  `<section class="pane glass" id="section">` elements the rows name. A pane's
  first `[data-select]` link is what Select does on that row, and the legend
  names it by its `data-select` text.
- **The start screen** is the list in `index.html`: a category is an `<li
  data-id data-glyph>` and a row an `<a data-glyph>` with a `.row-name` and a
  `.row-note`. The same list is the site map a browser without JavaScript sees,
  and what the search looks through.
- **A new page** also goes into `LXB.PAGES` in `shell.js`, which is what the
  guide lists.
- Keep what a page says in line with `docs/`: it is a summary of the guides,
  and every pane that has a guide links to it.

## The glyphs

A glyph in the shell is a white silhouette measured into a signed distance
field and stood up as a bead of water by the quad shader. A web page has
neither, so `site/tools/render-glyphs.py` does both offline, the same way:
rasterises the SVG at four times the 128-texel cell, takes the exact Euclidean
distance transform inside and out, stores it as `icons::distance_field` does,
shades it with a port of `glyph_material`, and writes a WebP at 320 and at 96
pixels. The material is white and does not depend on the accent, which is why a
picture of it can stand in for the shader.

```sh
python3 site/tools/render-glyphs.py              # every glyph in its list
python3 site/tools/render-glyphs.py steam logo   # just these
```

It needs `rsvg-convert`, NumPy, SciPy and Pillow. Add a glyph to its `GLYPHS`
list before using it on a page.

## The sounds

Every press on the site opens a new page, and a clip fetched the first time it
is wanted is a press that makes no sound. So the interface clips are carried in
`assets/js/sounds.js` as base64 MP3 and decoded as each page loads; a browser
lets a page make a sound only once it has been touched, and the first key,
click or tap of any kind wakes the audio device. The music is a file of its own,
on unless the visitor turns it off, and it carries on from page to page where
it had got to. A browser that refuses to start it before any input starts it at
the first one.

`site/tools/embed-sounds.py` makes both from the shell's own Ogg clips in
`crates/lxb-desktop/src/sounds`, with ffmpeg — the music only when asked, since
it is large and seldom changes:

```sh
python3 site/tools/embed-sounds.py            # the clips
python3 site/tools/embed-sounds.py --music    # and the music
```

Add a clip to its `CLIPS` list before playing it from `shell.js`.

## Languages

The site speaks the shell's ten languages, and the pages in British English at
the root of `site/` are the source of all the others. A button beside the
guide's, and a row in the guide itself, list them the way Settings > Language
does — each by its own name, in the order those names sort — and open the same
page, at the same place on it, in the language chosen. The choice is kept in
the visitor's browser. The first time a visit reaches the start screen, it opens
in that language, or else in the first one the browser asks for that the site
speaks; nowhere else redirects, so a page linked to in one language stays in it.

**Pages** are translated unit by unit. `site/tools/translate.py` reads each
English page and takes as a unit every element with text of its own — a
heading, a paragraph, a list item, a caption, a row's name and its note — whole,
with the markup inside it, plus the attributes a person reads (`alt`, `title`,
`aria-label`, `data-select`, a page's description). Each unit is known by a hash
of its English, and each language says what it says for it in
`site/tools/i18n/<language>/*.json`; `"="` means the unit is the same (a
product's name). Code, commands and anything marked `translate="no"` are never
units. American English is an overlay of the units the two Englishes spell
differently, as `en-US.ftl` is in the shell.

```sh
python3 site/tools/translate.py extract   # the English units, into i18n/en-GB
python3 site/tools/translate.py build     # every language's pages
python3 site/tools/translate.py check     # what is missing, and nothing else
```

Changing an English page changes the hashes of the units that changed, so
after an edit run `extract`, give each language its new units, and `build`; a
unit a language has not got yet stays in English there and is reported. `build`
also says where a translation does not carry the English's tags and links. The
pages in the language directories are generated: commit them, but never edit
them.

**The scripts' own words** — the legend, the guide, the search, the language
menu — are in `assets/js/i18n.js`, and where the shell has the same word they
are the shell's, from `crates/lxb-desktop/locales`. Every language addresses the
reader as the shell's catalog does.

**Fonts.** Roboto is served in three pieces by alphabet, and a page downloads
only the pieces its text uses. Hindi and Chinese are written in the Noto faces
the shell carries for them; the Han face is cut down to the characters the
Chinese pages use, so run `site/tools/subset-fonts.py` again after the Chinese
translation changes (it needs `pyftsubset` and `woff2_compress`).

## Search engines and language models

`translate.py build` also writes what search engines and language models read,
from `tools/seo.py`, so it is never out of step with the pages:

- **In every page's head**, between the two `search and sharing` comments: the
  page's own address (`canonical`), the same page in each of the other nine
  languages (`hreflang`, so a search offers the reader the one in their
  language, with the British English page as the default), the card a shared
  link unfolds into (Open Graph and Twitter), and schema.org data. The start
  screen describes LineXinBar as a `SoftwareApplication`, and every other page
  is a `WebPage` with a breadcrumb back to it. Never edit that block by hand;
  the build rewrites it.
- **`sitemap.xml`**, **`llms.txt`** and **`llms-full.txt`** beside the pages.
  The last two follow the llms.txt convention: a short summary with links, and
  the English text of every page in Markdown, for tools that read a site
  rather than render it.

The published address is `SITE_URL` at the top of `seo.py`. If the site moves
to a domain of its own, change it there and build again.

Each page's `<title>` says what is on it, not just the category's name
("Games on LineXinBar: Steam, Epic Games, RetroArch and PS3"), and a hidden
`<h1>` repeats it, since the visible name sits in the trail. The words people
search for (console-style desktop, XMB, XrossMediaBar, Steam, gamepad) belong
in the pages' own text and descriptions: that is what search engines rank on.

The start screen's first-visit language redirect leaves crawlers alone
(anything calling itself a bot, crawler or spider), so each language keeps
its own address in the index instead of the default one redirecting.

Once the site is published, add it to
[Google Search Console](https://search.google.com/search-console) as a URL
prefix property (`https://petexy.github.io/LineXinBar/`) and submit
`sitemap.xml` there. A `robots.txt` is only read at the root of a host, which
a project site on `github.io` does not own, so the sitemap has to be
submitted rather than announced. The same goes for Bing Webmaster Tools.

## Trying it locally

```sh
cd site && python3 -m http.server 8000
```

and open <http://localhost:8000>. Any static server will do, and opening
`site/index.html` straight from the disk works too, sounds included.

From the disk, Firefox gives every file a storage of its own, so a page could
not read what the page before it remembered. There the site hands everything
it keeps (the accent, the look, the volume, where the music and the wallpaper
had got to) to the next page in its address, as `?lxb=…`, and the page takes
it into its own storage and drops it from the address as it opens
(`LXB.carry` in `shell.js`). Every page the site's scripts open goes through
`LXB.carry`, and plain links are handed over as they are followed. The one
thing it cannot reach is the browser's own Back button, which reopens a page
as that file last had it. Served from a web address, the pages share one
storage and none of this happens; there a page brought back by Back, or open
in another tab, takes up whatever was chosen since.
