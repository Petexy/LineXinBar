#!/usr/bin/env python3
"""Keep an X server's pointer in its top-left corner, over nothing.

A nested shell photographed on a display the user's own mouse can reach has
its highlight moved by that mouse — see the memory of 2026-08-25 in
nested-shot.sh's history. Run this against a rootful Xwayland of the run's
own: it warps the pointer between (0,0) and (1,1) every half second, because a
warp to where the pointer already is generates no motion at all.

    python3 scripts/park-pointer.py :7 300 &
"""
import sys
import time

from Xlib import X, display
from Xlib.ext import xtest

d = display.Display(sys.argv[1])
end = time.time() + float(sys.argv[2])
flip = 0
while time.time() < end:
    xtest.fake_input(d, X.MotionNotify, x=flip, y=flip)
    d.sync()
    flip ^= 1
    time.sleep(0.5)
