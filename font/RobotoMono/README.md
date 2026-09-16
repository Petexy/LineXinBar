# Roboto Mono

Roboto Mono by Christian Robertson, licensed under the SIL Open Font
License 1.1 — see `OFL.txt`. Fetched from
<https://github.com/googlefonts/RobotoMono> (`fonts/ttf/RobotoMono-Regular.ttf`).

One face is compiled into `lxb-desktop` by `include_bytes!` (see
`MONO_FONT_REGULAR` in `crates/lxb-desktop/src/gpu.rs`):

* `RobotoMono-Regular.ttf`

It is the face of the one thing the shell draws that is not a label: the
transcript of what a package manager said, in the terminal frame of
Settings > Updates. A terminal's output is laid out in columns — pacman's
tables, apt's progress, a line of dashes under a heading — and a
proportional face turns those columns into a ragged edge. Embedded for the
reason Roboto is: the shell decides what it looks like, not fontconfig.
