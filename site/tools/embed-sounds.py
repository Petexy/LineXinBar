#!/usr/bin/env python3
"""
Makes the website's sounds from the shell's own clips.

The interface clips are written into assets/js/sounds.js as base64, so a page
has every one of them decoded before the first press rather than fetching each
the first time it is wanted — which is a press that makes no sound, and on a
site where every press opens a new page, that was most of them. A script is
also the one kind of file a browser will read from a page opened straight off
the disk, where fetching a sound is refused.

The music is too long to carry that way and stays a file of its own, made
again only when asked for, since it is large and seldom changes.

Both are MP3, the one format every browser decodes: the shell's are Ogg Vorbis.

    python3 site/tools/embed-sounds.py            # the clips
    python3 site/tools/embed-sounds.py --music    # and the music

Needs ffmpeg.
"""

import base64
import pathlib
import subprocess
import sys

TOOLS = pathlib.Path(__file__).resolve().parent
SITE = TOOLS.parent
SHELL_SOUNDS = SITE.parent / "crates" / "lxb-desktop" / "src" / "sounds"

# The clips the site spends, each where the shell spends it — see shell.js.
CLIPS = [
    "press",
    "press-selected",
    "press-back",
    "guide-open",
    "press-guide",
    "press-guide-selected",
    "app-launch",
]


def mp3(source: pathlib.Path, bitrate: str) -> bytes:
    return subprocess.run(
        [
            "ffmpeg", "-v", "error", "-i", str(source),
            "-map_metadata", "-1", "-codec:a", "libmp3lame", "-b:a", bitrate,
            "-f", "mp3", "-",
        ],
        check=True,
        capture_output=True,
    ).stdout


def main() -> None:
    lines = [
        "/*",
        " * The shell's interface clips, as MP3 in base64. Made by",
        " * site/tools/embed-sounds.py from crates/lxb-desktop/src/sounds — edit",
        " * those and run it again rather than editing this.",
        " */",
        "window.LXB_SOUNDS = {",
    ]
    total = 0
    for name in CLIPS:
        data = mp3(SHELL_SOUNDS / f"{name}.ogg", "112k")
        total += len(data)
        lines.append(f'  "{name}": "{base64.b64encode(data).decode("ascii")}",')
    lines.append("};")
    out = SITE / "assets" / "js" / "sounds.js"
    out.write_text("\n".join(lines) + "\n")
    print(f"{out.relative_to(SITE)}: {len(CLIPS)} clips, {total // 1024} KiB of MP3")

    if "--music" not in sys.argv[1:]:
        return
    music = SITE / "assets" / "sounds" / "start-bg-music.mp3"
    music.parent.mkdir(parents=True, exist_ok=True)
    music.write_bytes(mp3(SHELL_SOUNDS / "start-bg-music.ogg", "128k"))
    print(f"{music.relative_to(SITE)}: {music.stat().st_size // 1024} KiB")


if __name__ == "__main__":
    main()
