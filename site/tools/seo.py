"""
What search engines and language models read of the website, written by
`translate.py build` so that it can never fall behind the pages.

Every page, in every language, gets a block in its <head>: the address it is
known by, the same page's address in each of the other nine languages (so a
search shows the reader the one in their language), the card a link to it
unfolds into when shared, and a description of it in schema.org's terms for
the machines that read those. Beside the pages, the build writes:

    sitemap.xml     every page in every language, each with its translations
    llms.txt        the site in a page of Markdown, for language models
    llms-full.txt   the English text of every page, as Markdown

The published address is SITE_URL below. Change it there, and only there, if
the site moves to a domain of its own.
"""

import json
import pathlib
import re
from urllib.parse import urljoin

from bs4 import BeautifulSoup, NavigableString, Tag

SITE_URL = "https://petexy.github.io/LineXinBar/"
REPO = "https://github.com/Petexy/LineXinBar"

TOOLS = pathlib.Path(__file__).resolve().parent
SITE = TOOLS.parent

# Every language the site is in, with its directory and how Open Graph writes it.
LANGUAGES = [
    ("de", "de", "de_DE", "Deutsch"),
    ("en-GB", "", "en_GB", "English (UK)"),
    ("en-US", "en-us", "en_US", "English (US)"),
    ("es", "es", "es_ES", "Español"),
    ("fr", "fr", "fr_FR", "Français"),
    ("pl", "pl", "pl_PL", "Polski"),
    ("pt-BR", "pt-br", "pt_BR", "Português (Brasil)"),
    ("ru", "ru", "ru_RU", "Русский"),
    ("hi", "hi", "hi_IN", "हिन्दी"),
    ("zh-CN", "zh-cn", "zh_CN", "简体中文"),
]

# Words a search for this kind of desktop is made of, for the machines that
# read a list of them. Search engines rank on the pages' own text, not this.
KEYWORDS = [
    "LineXinBar", "XMB", "XrossMediaBar", "cross-media bar", "console-style desktop",
    "Linux desktop", "Wayland compositor", "gamepad", "controller", "couch gaming",
    "HTPC", "Steam", "Epic Games", "RetroArch", "RPCS3", "PlayStation 3",
]

SHARE_IMAGE = ("assets/img/desktop.png", 1600, 900)

BEGIN = "<!-- search and sharing: written by tools/translate.py build -->"
END = "<!-- /search and sharing -->"
BLOCK = re.compile(re.escape(BEGIN) + r".*?" + re.escape(END) + r"\n?", re.S)
DESCRIPTION = re.compile(r'<meta[^>]*name="description"[^>]*>\n?')


def version() -> str:
    shell = (SITE / "assets" / "js" / "shell.js").read_text(encoding="utf-8")
    return re.search(r'LXB\.VERSION = "([^"]+)"', shell).group(1)


def address(directory: str, page: str) -> str:
    """Where a page is published: a language's start screen by its directory."""
    return SITE_URL + (directory + "/" if directory else "") + ("" if page == "index.html" else page)


def strip(text: str) -> str:
    """A page without its block, as the translations are made from it."""
    return BLOCK.sub("", text)


def attribute(value: str) -> str:
    return value.replace("&", "&amp;").replace('"', "&quot;").replace("<", "&lt;")


def facts(soup: BeautifulSoup):
    """The title, the description, and the two names of the page's trail."""
    title = soup.title.get_text(" ", strip=True)
    meta = soup.find("meta", attrs={"name": "description"})
    description = meta["content"].strip() if meta else ""
    back = soup.select_one(".trail-back")
    name = soup.select_one(".trail-name")
    home = back.get_text(" ", strip=True).lstrip("‹").strip() if back else ""
    return title, description, home, name.get_text(" ", strip=True) if name else ""


def structured(page, tag, directory, title, description, home, name) -> dict:
    here = address(directory, page)
    start = address(directory, "index.html")
    if page == "index.html":
        image, width, height = SHARE_IMAGE
        return {"@context": "https://schema.org", "@graph": [
            {"@type": "WebSite", "@id": SITE_URL + "#website", "url": start, "name": "LineXinBar",
             "description": description, "inLanguage": tag},
            {"@type": "SoftwareApplication", "@id": SITE_URL + "#software", "name": "LineXinBar",
             "alternateName": ["LXB", "LineXinBar XMB"], "description": description, "url": start,
             "applicationCategory": "DesktopEnhancementApplication",
             "applicationSubCategory": "Desktop environment", "operatingSystem": "Linux",
             "softwareVersion": version(), "license": "https://www.gnu.org/licenses/gpl-3.0.html",
             "isAccessibleForFree": True,
             "offers": {"@type": "Offer", "price": "0", "priceCurrency": "USD"},
             "downloadUrl": REPO + "/releases", "installUrl": address(directory, "install.html"),
             "screenshot": {"@type": "ImageObject", "url": SITE_URL + image, "width": width, "height": height},
             "inLanguage": [t for t, *_ in LANGUAGES],
             "keywords": ", ".join(KEYWORDS),
             "author": {"@type": "Person", "name": "Petexy", "url": "https://github.com/Petexy"},
             "sameAs": [REPO]},
        ]}
    return {"@context": "https://schema.org", "@type": "WebPage", "@id": here, "url": here,
            "name": title, "description": description, "inLanguage": tag,
            "isPartOf": {"@id": SITE_URL + "#website"}, "about": {"@id": SITE_URL + "#software"},
            "breadcrumb": {"@type": "BreadcrumbList", "itemListElement": [
                {"@type": "ListItem", "position": 1, "name": home or "LineXinBar", "item": start},
                {"@type": "ListItem", "position": 2, "name": name or title, "item": here},
            ]}}


def block(page: str, tag: str, directory: str, soup: BeautifulSoup) -> str:
    title, description, home, name = facts(soup)
    here = address(directory, page)
    locale = next(l for t, _d, l, _n in LANGUAGES if t == tag)
    image, width, height = SHARE_IMAGE
    lines = [BEGIN, f'<link rel="canonical" href="{here}">',
             f'<link rel="alternate" hreflang="x-default" href="{address("", page)}">',
             f'<link rel="alternate" hreflang="en" href="{address("", page)}">']
    lines += [f'<link rel="alternate" hreflang="{t}" href="{address(d, page)}">' for t, d, _l, _n in LANGUAGES]
    lines += [
        '<meta property="og:type" content="website">',
        '<meta property="og:site_name" content="LineXinBar">',
        f'<meta property="og:title" content="{attribute(title)}">',
        f'<meta property="og:description" content="{attribute(description)}">',
        f'<meta property="og:url" content="{here}">',
        f'<meta property="og:locale" content="{locale}">',
        f'<meta property="og:image" content="{SITE_URL + image}">',
        f'<meta property="og:image:width" content="{width}">',
        f'<meta property="og:image:height" content="{height}">',
        '<meta name="twitter:card" content="summary_large_image">',
    ]
    data = json.dumps(structured(page, tag, directory, title, description, home, name),
                      ensure_ascii=False, separators=(",", ":")).replace("</", "<\\/")
    lines += [f'<script type="application/ld+json">{data}</script>', END]
    return "\n".join(lines) + "\n"


def place(text: str, page: str, tag: str, directory: str) -> str:
    """The page with its block, fresh, straight after its description."""
    text = strip(text)
    soup = BeautifulSoup(text, "html.parser")
    found = DESCRIPTION.search(text)
    at = found.end() if found else text.index("</title>") + len("</title>\n")
    if found and not found.group(0).endswith("\n"):
        return text[:at] + "\n" + block(page, tag, directory, soup) + text[at:]
    return text[:at] + block(page, tag, directory, soup) + text[at:]


def sitemap(pages) -> str:
    lines = ['<?xml version="1.0" encoding="UTF-8"?>',
             '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9" '
             'xmlns:xhtml="http://www.w3.org/1999/xhtml">']
    for page in pages:
        for _t, directory, _l, _n in LANGUAGES:
            lines.append(f"  <url>\n    <loc>{address(directory, page)}</loc>")
            lines.append(f'    <xhtml:link rel="alternate" hreflang="x-default" href="{address("", page)}"/>')
            for t, d, _l2, _n2 in LANGUAGES:
                lines.append(f'    <xhtml:link rel="alternate" hreflang="{t}" href="{address(d, page)}"/>')
            lines.append("  </url>")
    lines.append("</urlset>")
    return "\n".join(lines) + "\n"


# --- Markdown, for language models ------------------------------------------------

def inline(node, base: str) -> str:
    if isinstance(node, NavigableString):
        return re.sub(r"\s+", " ", str(node))
    if not isinstance(node, Tag) or node.name in ("script", "style", "button"):
        return ""
    if node.name == "img":
        return ""
    if node.name == "br":
        return "\n"
    inner = "".join(inline(c, base) for c in node.children)
    if node.name in ("strong", "b"):
        return f"**{inner.strip()}**" if inner.strip() else ""
    if node.name == "em":
        return f"*{inner.strip()}*" if inner.strip() else ""
    if node.name in ("code", "kbd", "samp"):
        return f"`{node.get_text()}`"
    if node.name == "a" and node.get("href"):
        return f"[{inner.strip()}]({urljoin(base, node['href'])})"
    return inner


def blocks(el: Tag, base: str, out: list):
    for child in el.children:
        if isinstance(child, NavigableString):
            if child.strip():
                out.append(re.sub(r"\s+", " ", child.strip()))
            continue
        if not isinstance(child, Tag) or child.name in ("script", "style", "button", "nav"):
            continue
        cls = child.get("class") or []
        if "more" in cls or "visually-hidden" in cls:
            continue
        name = child.name
        if "cards" in cls:
            items = []
            for card in child.select(".card"):
                title = card.find("b")
                text = card.find("span")
                label = title.get_text(" ", strip=True) if title else ""
                about = inline(text, base).strip() if text else ""
                link = f"[{label}]({urljoin(base, card['href'])})" if card.get("href") else label
                items.append(f"- {link}: {about}" if about else f"- {link}")
            out.append("\n".join(items))
        elif "tab-panel" in cls:
            # A tab's contents under the tab's name, since the tabs themselves
            # are buttons: "Arch", then Arch's commands.
            tab = child.find_parent("html").find(id=child.get("aria-labelledby", ""))
            if tab:
                out.append(f"**{tab.get_text(' ', strip=True)}:**")
            blocks(child, base, out)
        elif name in ("h2", "h3", "h4"):
            out.append("#" * int(name[1]) + " " + inline(child, base).strip())
        elif name == "p":
            text = inline(child, base).strip()
            if text:
                out.append(text)
        elif name in ("ul", "ol"):
            items = child.find_all("li", recursive=False)
            out.append("\n".join(("1. " if name == "ol" else "- ") + inline(li, base).strip() for li in items))
        elif name == "pre":
            out.append("```\n" + child.get_text().strip("\n") + "\n```")
        elif name == "img" and "shot" in cls:
            out.append(f"![{child.get('alt', '')}]({urljoin(base, child['src'])})")
        elif name == "figcaption":
            out.append("*" + inline(child, base).strip() + "*")
        elif name == "table":
            rows = [[inline(c, base).strip().replace("|", "\\|") for c in tr.find_all(["th", "td"])]
                    for tr in child.find_all("tr")]
            if rows:
                width = max(len(r) for r in rows)
                rows = [r + [""] * (width - len(r)) for r in rows]
                table = ["| " + " | ".join(rows[0]) + " |", "|" + " --- |" * width]
                table += ["| " + " | ".join(r) + " |" for r in rows[1:]]
                out.append("\n".join(table))
        else:
            blocks(child, base, out)


def page_markdown(page: str, text: str) -> str:
    soup = BeautifulSoup(text, "html.parser")
    title, description, _home, _name = facts(soup)
    base = address("", page)
    out = [f"# {title}", f"Source: {base}", description]
    for pane in soup.select(".pane"):
        blocks(pane, base, out)
    return "\n\n".join(out) + "\n"


def llms(pages, read) -> tuple:
    """llms.txt and llms-full.txt, from the English pages."""
    start = BeautifulSoup(read("index.html"), "html.parser")
    _t, summary, _h, _n = facts(start)
    lines = [
        "# LineXinBar", "", f"> {summary}", "",
        f"LineXinBar is free software under the GNU GPL 3.0, at version {version()} and in early "
        "development. It is a whole Linux session — compositor, launcher, file manager, settings and "
        "prompts — laid out as an XMB (a cross-media bar in the manner of the PlayStation 3's "
        "XrossMediaBar) and made to be driven from the couch with a gamepad, as well as with a "
        "keyboard, a mouse or a touchscreen. The site and the shell are in ten languages.", "",
        "## Pages", "",
    ]
    full = []
    for page in pages:
        if page == "index.html":
            continue
        text = read(page)
        title, description, _h, _n = facts(BeautifulSoup(text, "html.parser"))
        lines.append(f"- [{title}]({address('', page)}): {description}")
        full.append(page_markdown(page, text))
    lines += ["", "## Source and documentation", "",
              f"- [Source code on GitHub]({REPO}): the compositor and the shell, in Rust",
              f"- [Documentation]({REPO}/tree/HEAD/docs): installing, using, building and translating LineXinBar",
              f"- [Releases]({REPO}/releases): the published versions and their packages",
              f"- [Issues]({REPO}/issues): reporting a bug",
              "", "## In other languages", ""]
    lines += [f"- [{n}]({address(d, 'index.html')})" for t, d, _l, n in LANGUAGES if t != "en-GB"]
    lines += ["", "## Optional", "",
              f"- [Every page in full]({SITE_URL}llms-full.txt): the English text of the whole site, as Markdown", ""]
    head = f"# LineXinBar — the whole site\n\n> {summary}\n\n"
    return "\n".join(lines), head + "\n---\n\n".join(full)


def write_all(pages, read):
    (SITE / "sitemap.xml").write_text(sitemap(pages), encoding="utf-8")
    short, full = llms(pages, read)
    (SITE / "llms.txt").write_text(short, encoding="utf-8")
    (SITE / "llms-full.txt").write_text(full, encoding="utf-8")
