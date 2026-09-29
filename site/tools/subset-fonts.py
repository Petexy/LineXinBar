#!/usr/bin/env python3
"""
The website's fonts, cut from the shell's own (font/ at the repository root).

Roboto in three pieces by alphabet — Latin, the rest of Latin (Polish,
say), and Cyrillic — so a page downloads only the pieces its text uses. The
shell carries Noto Sans Devanagari UI and Noto Sans CJK SC for Hindi and
Chinese, and so does the site: the Devanagari face whole, since the script
is small, and the Han face cut down to the characters the Chinese pages
actually use, since the whole of it is sixteen megabytes. The language menu
writes हिन्दी and 简体中文 on every page, so those two names get a face of their
own a few kilobytes big, and a page in any language can draw its menu
without fetching a whole script for it.

Run it again after the Chinese translation changes:

    python3 site/tools/subset-fonts.py

Needs fontTools (pyftsubset) with Brotli, and BeautifulSoup 4.
"""

import pathlib
import re
import subprocess

from bs4 import BeautifulSoup

TOOLS = pathlib.Path(__file__).resolve().parent
SITE = TOOLS.parent
FONTS = SITE.parent / "font"
OUT = SITE / "assets" / "fonts"

# The pieces Roboto is cut into. What Google Fonts serves Roboto as, with the
# arrows and the triangle the pages write in their keys added to the Latin one.
RANGES = {
    "latin": "U+0000-00FF,U+0131,U+0152-0153,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0304,U+0308,"
             "U+0329,U+2000-206F,U+20AC,U+2122,U+2190-2199,U+2212,U+2215,U+25B3,U+FEFF,U+FFFD",
    "latin-ext": "U+0100-02BA,U+02BD-02C5,U+02C7-02CC,U+02CE-02D7,U+02DD-02FF,U+1D00-1DBF,"
                 "U+1E00-1E9F,U+1EF2-1EFF,U+2020,U+20A0-20AB,U+20AD-20C0,U+2113,U+2C60-2C7F,U+A720-A7FF",
    "cyrillic": "U+0301,U+0400-045F,U+0490-0491,U+04B0-04B1,U+2116",
}
DEVANAGARI = "U+0900-097F,U+1CD0-1CF9,U+200C-200D,U+20A8,U+20B9,U+25CC,U+A830-A839,U+A8E0-A8FF"
NAMES = "हिन्दी简体中文"


def subset(source: pathlib.Path, target: pathlib.Path, *, unicodes=None, text=None):
    # Cut to a TrueType file and compressed with woff2_compress, which needs
    # nothing of Python's; fontTools' own WOFF2 writer wants the Brotli module.
    cut = target.with_suffix(".ttf")
    args = ["pyftsubset", str(source), f"--output-file={cut}",
            "--layout-features=*", "--no-hinting", "--desubroutinize"]
    if unicodes:
        args.append(f"--unicodes={unicodes}")
    if text is not None:
        args.append(f"--text={text}")
    subprocess.run(args, check=True, stderr=subprocess.DEVNULL)
    subprocess.run(["woff2_compress", str(cut)], check=True, stdout=subprocess.DEVNULL)
    cut.unlink()
    print(f"{target.relative_to(SITE)}: {target.stat().st_size // 1024} KiB")


def chinese_text() -> str:
    """Every character the Chinese pages and the scripts' Chinese words use."""
    seen = set(NAMES)
    for page in (SITE / "zh-cn").glob("*.html"):
        soup = BeautifulSoup(page.read_text(encoding="utf-8"), "html.parser")
        seen.update(soup.get_text())
        for el in soup.find_all(True):
            for attr in ("alt", "title", "aria-label", "data-select", "content"):
                if isinstance(el.get(attr), str):
                    seen.update(el[attr])
    words = (SITE / "assets" / "js" / "i18n.js").read_text(encoding="utf-8")
    block = words[words.index('"zh-CN": {'):]
    seen.update(block[:block.index("\n    },")])
    # Full-width punctuation the translations may come to use.
    seen.update("，。、：；！？（）「」『』“”‘’《》〈〉【】…—·～")
    return "".join(sorted(c for c in seen if ord(c) >= 0x2E80))


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for weight in ("Regular", "Medium", "Bold"):
        for name, unicodes in RANGES.items():
            subset(FONTS / "Roboto" / "static" / f"Roboto-{weight}.ttf",
                   OUT / f"Roboto-{weight}-{name}.woff2", unicodes=unicodes)
    for weight in ("Regular", "Bold"):
        subset(FONTS / "NotoSansDevanagariUI" / f"NotoSansDevanagariUI-{weight}.ttf",
               OUT / f"NotoSansDevanagariUI-{weight}.woff2", unicodes=DEVANAGARI)
    han = chinese_text()
    print(f"{len(han)} Han characters and marks on the Chinese pages")
    for weight in ("Regular", "Bold"):
        subset(FONTS / "NotoSansCJKsc" / f"NotoSansCJKsc-{weight}.ttf",
               OUT / f"NotoSansCJKsc-{weight}.woff2", text=han)
    subset(FONTS / "NotoSansDevanagariUI" / "NotoSansDevanagariUI-Regular.ttf",
           OUT / "LanguageNames-Devanagari.woff2", text="हिन्दी")
    subset(FONTS / "NotoSansCJKsc" / "NotoSansCJKsc-Regular.ttf",
           OUT / "LanguageNames-Han.woff2", text="简体中文")


if __name__ == "__main__":
    main()
