#!/usr/bin/env python3
"""
The website in every language the shell speaks.

The English pages in site/ are the source. Every piece of text on them — a
heading, a paragraph, a list item, a caption, a row's name and its note, and
the attributes a person reads (alt, title, aria-label, data-select, a page's
description) — is a *unit*, identified by a hash of its English, and each
language keeps what it says for each unit in site/tools/i18n/<lang>/*.json:

    { "3f2a9c01d4": "Übersetzung, mit <strong>denselben</strong> Tags" }

A value of "=" means the unit is the same in that language (a product's
name, say). `build` writes each language's copy of every page into its own
directory — de/games.html beside games.html — with the page's own paths
rewritten and its `lang` set; a unit a language has not got yet stays in
English there, and is reported. Code, commands and anything marked
translate="no" are never units. `build` also writes what search engines and
language models read — see seo.py.

A unit is the outermost element with text of its own, taken whole with the
markup inside it; a glyph at its head is left out of it, since it says
nothing. So a translation carries the same inline tags as the English — the
same links to the same places — and `build` says where one does not.

    python3 site/tools/translate.py extract   # the English, page by page
    python3 site/tools/translate.py build     # every language's pages
    python3 site/tools/translate.py check     # what is missing, and nothing else

Needs BeautifulSoup 4.
"""

import hashlib
import json
import pathlib
import re
import sys
from collections import Counter

from bs4 import BeautifulSoup, NavigableString, Tag, Comment

import seo

TOOLS = pathlib.Path(__file__).resolve().parent
SITE = TOOLS.parent
CATALOGS = TOOLS / "i18n"

# The languages the shell speaks (crates/lxb-desktop/src/i18n.rs), with the
# directory each one's pages go in. British English is the source, at the root.
LANGUAGES = [
    ("de", "de"),
    ("en-US", "en-us"),
    ("es", "es"),
    ("fr", "fr"),
    ("pl", "pl"),
    ("pt-BR", "pt-br"),
    ("ru", "ru"),
    ("hi", "hi"),
    ("zh-CN", "zh-cn"),
]

# Pages that are copied. The 404 page is not: GitHub Pages serves one, from
# the root, for every missing address, and it translates itself.
PAGES = [
    "index.html", "overview.html", "install.html", "games.html", "media.html",
    "guide.html", "displays.html", "settings.html", "controls.html",
    "desktop.html", "family.html", "develop.html",
]

NEVER = {"script", "style", "pre", "code", "kbd", "samp", "noscript", "template", "svg", "canvas"}
ATTRIBUTES = ("alt", "title", "aria-label", "data-select", "placeholder")
# A page's sharing card is written from its title and description by the
# build (seo.py), so those two are the only words in its head.
META = {("name", "description")}


def unit_id(text: str) -> str:
    return hashlib.sha1(text.encode("utf-8")).hexdigest()[:10]


def normal(html: str) -> str:
    return re.sub(r"\s+", " ", html).strip()


def own_text(el: Tag) -> bool:
    return any(isinstance(c, NavigableString) and not isinstance(c, Comment) and c.strip() for c in el.children)


def is_glyph(node) -> bool:
    return isinstance(node, Tag) and node.name == "img" and "glyph" in (node.get("class") or [])


def span_of(el: Tag):
    """The children a unit covers: all of them but a glyph at its head, and
    the whitespace round the ends."""
    kids = list(el.children)
    while kids and (is_glyph(kids[0]) or (isinstance(kids[0], NavigableString) and not kids[0].strip())):
        kids.pop(0)
    while kids and isinstance(kids[-1], NavigableString) and not kids[-1].strip():
        kids.pop()
    return kids


def units(soup: BeautifulSoup):
    """Every unit on a page, in order: ("text", element, children, english)
    and ("attr", element, attribute, english)."""
    found = []

    def walk(el: Tag):
        if el.name in NEVER or el.get("translate") == "no":
            return
        for attr in ATTRIBUTES:
            value = el.get(attr)
            if isinstance(value, str) and value.strip():
                found.append(("attr", el, attr, normal(value)))
        if el.name == "meta":
            for key, value in META:
                if el.get(key) == value and el.get("content", "").strip():
                    found.append(("attr", el, "content", normal(el["content"])))
            return
        if own_text(el):
            kids = span_of(el)
            english = normal("".join(str(k) for k in kids))
            if english:
                found.append(("text", el, kids, english))
            # What is inside a unit — a link, an image's alt — is translated
            # with it, as part of its text.
            return
        for child in el.children:
            if isinstance(child, Tag):
                walk(child)

    walk(soup.html)
    return found


def source(page: str) -> str:
    """An English page as it was written, without what the build adds to it."""
    return seo.strip((SITE / page).read_text(encoding="utf-8"))


def read(page: str) -> BeautifulSoup:
    return BeautifulSoup(source(page), "html.parser")


def extract():
    seen = set()
    out = CATALOGS / "en-GB"
    out.mkdir(parents=True, exist_ok=True)
    for old in out.glob("*.json"):
        old.unlink()
    total = 0
    for page in PAGES:
        entries = {}
        for kind, _el, _where, english in units(read(page)):
            uid = unit_id(english)
            if uid in seen:
                continue
            seen.add(uid)
            entries[uid] = english
        total += len(entries)
        (out / page.replace(".html", ".json")).write_text(
            json.dumps(entries, ensure_ascii=False, indent=1) + "\n", encoding="utf-8")
        print(f"{page:16} {len(entries):4} new units")
    print(f"{total} units in all")


def catalog(tag: str) -> dict:
    merged = {}
    for f in sorted((CATALOGS / tag).glob("*.json")):
        merged.update(json.loads(f.read_text(encoding="utf-8")))
    return merged


def markup(html: str) -> Counter:
    """The tags a piece of text carries, and where its links go — what a
    translation has to keep."""
    soup = BeautifulSoup(html, "html.parser")
    return Counter((t.name, t.get("href", "")) for t in soup.find_all(True))


def rewrite_paths(soup: BeautifulSoup):
    for el in soup.find_all(True):
        for attr in ("src", "href", "content"):
            value = el.get(attr)
            if isinstance(value, str) and value.startswith("assets/"):
                el[attr] = "../" + value


def build(write=True):
    everything = set()
    for page in PAGES:
        everything.update(unit_id(u[3]) for u in units(read(page)))
    problems = 0
    for tag, directory in LANGUAGES:
        words = catalog(tag)
        missing, wrong = set(), []
        for page in PAGES:
            soup = read(page)
            for kind, el, where, english in units(soup):
                uid = unit_id(english)
                said = words.get(uid)
                if said is None:
                    # British English and American English differ in a few
                    # words only, so American takes British where it has
                    # nothing of its own to say.
                    if tag != "en-US":
                        missing.add(uid)
                    continue
                if said == "=":
                    continue
                if kind == "attr":
                    el[where] = said
                    continue
                if markup(said) != markup(english):
                    wrong.append((page, uid))
                fragment = BeautifulSoup(said, "html.parser")
                anchor = where[0]
                for node in list(fragment.contents):
                    anchor.insert_before(node)
                for node in where:
                    node.extract()
            soup.html["lang"] = tag
            rewrite_paths(soup)
            if write:
                target = SITE / directory / page
                target.parent.mkdir(exist_ok=True)
                target.write_text(seo.place(str(soup), page, tag, directory), encoding="utf-8")
        stale = set(words) - everything
        problems += len(missing) + len(wrong)
        line = f"{tag:6} {len(words) - len(stale & set(words)):5} units"
        if missing:
            line += f", {len(missing)} missing"
        if stale:
            line += f", {len(stale)} no longer on any page"
        print(line)
        for page, uid in wrong[:20]:
            print(f"       {page}: {uid} does not carry the English's tags and links")
    if write:
        # The English pages take their block in place, the rest of them as
        # they were written; then the files beside the pages.
        for page in PAGES:
            path = SITE / page
            text = path.read_text(encoding="utf-8")
            fresh = seo.place(text, page, "en-GB", "")
            if fresh != text:
                path.write_text(fresh, encoding="utf-8")
        seo.write_all(PAGES, source)
        print("sitemap.xml, llms.txt and llms-full.txt written")
    return problems


if __name__ == "__main__":
    command = sys.argv[1] if len(sys.argv) > 1 else "build"
    if command == "extract":
        extract()
    elif command == "build":
        build()
    elif command == "check":
        sys.exit(1 if build(write=False) else 0)
    else:
        sys.exit(__doc__)
