# Noto Sans Devanagari UI

Noto Sans Devanagari UI by the Noto Project Authors, licensed under the SIL
Open Font License 1.1 — see `OFL.txt`. The two files here are the ones
ConsoleExperienceDesktopManager bundles under `assets/fonts/`, byte for byte:
the whole Devanagari block with every conjunct the script is shaped with,
and nothing of any other script.

Two faces are compiled into `lxb-desktop` by `include_bytes!` (see
`UI_FONT_FALLBACKS` in `crates/lxb-desktop/src/gpu.rs`):

* `NotoSansDevanagariUI-Regular.ttf`
* `NotoSansDevanagariUI-Bold.ttf`

Roboto has no Devanagari in it, and a shell in Hindi is written in nothing
else. Embedded for the reason Roboto is: the shell's own words have to draw
on a machine with no fonts installed at all, and the *UI* cut is the one
whose ascenders and descenders fit the line height every row here was
measured against. The Settings > Language row that says हिन्दी is drawn from
this face whatever language the shell is in.
