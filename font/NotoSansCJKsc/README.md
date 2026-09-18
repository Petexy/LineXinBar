# Noto Sans CJK SC

Noto Sans CJK SC by Adobe and Google, licensed under the SIL Open Font
License 1.1 — see `OFL.txt`. Cut by `scripts/subset-han-face.py` from the
collection a distribution installs as `noto-fonts-cjk`
(`/usr/share/fonts/noto-cjk/NotoSansCJK-{Regular,Bold}.ttc`, the SC face).

Two faces are compiled into `lxb-desktop` by `include_bytes!` (see
`UI_FONT_FALLBACKS` in `crates/lxb-desktop/src/gpu.rs`):

* `NotoSansCJKsc-Regular.ttf`
* `NotoSansCJKsc-Bold.ttf`

Roboto has no Han in it, and the whole of Noto Sans CJK is twenty megabytes
a weight, so this is a subset: the 6,763 characters of GB 2312 — the
character set of every Chinese computer for two decades, and still what
"the characters" means in everyday use — plus the punctuation Chinese is
written with, in a megabyte and a half a weight. The standard's first level
alone, the 3,755 most frequent, was tried first and fell five characters
short of the catalogue, one of them the 浏 of 浏览器, *browser*, and three of
them countries. Every word of `locales/zh-CN.ftl` is held to the set by a
test; a character outside it is a sentence to reword. A name from elsewhere
draws from the machine's own fonts, as a name in any script Roboto has not
got does.

Regenerate with `python3 scripts/subset-han-face.py`; the same two files are
transcribed into lxb-toolkit and held byte for byte by its sync check.
