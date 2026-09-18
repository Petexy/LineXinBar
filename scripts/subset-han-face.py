#!/usr/bin/env python3
"""Cut the shell's Han face out of Noto Sans CJK SC.

Roboto has no Han in it, and the whole of Noto Sans CJK is twenty megabytes
a weight, so what travels with the shell is a subset: the 6,763 characters
of GB 2312, which was the character set of every Chinese computer for two
decades and is still what "the characters" means in everyday use, plus the
punctuation Chinese is written with. Its first level alone — the 3,755 most
frequent — was tried first and fell five characters short of the catalog,
one of them the 浏 of 浏览器, *browser*, and three of them countries; a cut
that has to be extended for a browser is not a cut. Every word in
`zh-CN.ftl` is held to the set by
`gpu::tests::every_catalog_word_is_drawn_by_a_bundled_face`; a character
outside it is a sentence to reword, not a face to regrow. A name from
elsewhere — a game's, a file's — draws from the machine's own fonts, as it
does in every other script Roboto has not got.

The same two files are transcribed into lxb-toolkit byte for byte and held
there by scripts/check-sync.sh, so a change here is a change in both.

Needs fonttools (`pyftsubset`) and the Noto CJK collection a distribution
installs as noto-fonts-cjk:

    python3 scripts/subset-han-face.py [/usr/share/fonts/noto-cjk]
"""
import pathlib
import subprocess
import sys

from fontTools.ttLib import TTCollection

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "font" / "NotoSansCJKsc"
SOURCE = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "/usr/share/fonts/noto-cjk")

# GB 2312-80: rows 16 to 87 of the table hold its 6,763 characters, level
# one (rows 16 to 55, by pinyin) and level two (56 to 87, by radical). The
# codec's own knowledge of the standard, so no list is kept here.
characters = []
for row in range(0xB0, 0xF8):
    for column in range(0xA1, 0xFF):
        try:
            characters.append(bytes([row, column]).decode("gb2312"))
        except UnicodeDecodeError:
            pass
assert len(characters) == 6763, len(characters)

# ASCII, so a digit or a Latin letter inside a Chinese run still comes from
# one face; CJK symbols and punctuation (。、「」…); the fullwidth forms Chinese
# punctuation is written in (，：；！？（）); and the few general marks that
# have no fullwidth form of their own.
ranges = "U+0020-007E,U+3000-303F,U+FF01-FF5E,U+2014,U+2018,U+2019,U+201C,U+201D,U+2026,U+00B7"

text = OUT / "gb2312.txt"
text.write_text("".join(characters))
for weight in ["Regular", "Bold"]:
    collection = SOURCE / f"NotoSansCJK-{weight}.ttc"
    faces = [font["name"].getDebugName(1) for font in TTCollection(collection).fonts]
    index = faces.index("Noto Sans CJK SC")
    subprocess.run(
        [
            "pyftsubset",
            str(collection),
            f"--font-number={index}",
            f"--text-file={text}",
            f"--unicodes={ranges}",
            f"--output-file={OUT / f'NotoSansCJKsc-{weight}.ttf'}",
            # No feature closure: the alternates a vertical or a stylistic
            # feature would pull in are glyphs the shell never asks for, and
            # they are most of the font.
            "--layout-features=",
            "--no-layout-closure",
            "--drop-tables+=vhea,vmtx,VORG,BASE",
            "--no-hinting",
        ],
        check=True,
    )
text.unlink()
for weight in ["Regular", "Bold"]:
    path = OUT / f"NotoSansCJKsc-{weight}.ttf"
    print(f"{path.relative_to(ROOT)}: {path.stat().st_size} bytes")
