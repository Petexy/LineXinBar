# Roboto

Roboto by Christian Robertson, licensed under the Apache License 2.0 — see
`LICENSE.txt`.

Two of these faces are compiled into `linboard-xmb` by `include_bytes!` (see
`UI_FONT_REGULAR` and `UI_FONT_BOLD` in `crates/linboard-xmb/src/gpu.rs`):

* `static/Roboto-Regular.ttf`
* `static/Roboto-Bold.ttf`

They are embedded rather than installed because the shell draws its first
frame before anything about the machine is guaranteed, including which fonts
exist. Everything else in `static/` is here for completeness and is not built
into anything; the shell only ever asks for normal and bold weights.
