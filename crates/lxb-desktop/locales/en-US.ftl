# English, as America writes it.
#
# An overlay rather than a catalog. Every message not written here is answered
# out of `en-GB.ftl`, which is the English the shell is written in and the one
# every other language falls back to — so what belongs in this file is only
# what the two Englishes really disagree about, and nothing else. A message
# copied here unchanged is a message that has to be edited twice from the day
# it is copied, and a translator who reads this file should be able to see the
# whole of the difference in one screen.
#
# Two kinds of thing are in it: the order a date is written in, and the handful
# of words the two spell differently. See `i18n::AMERICAN`, and
# `docs/localization.md` for how a third English would be added.
#
# The message identifiers are the British ones — they are names, fixed by the
# code that asks for them, and `shell-the-colour-of-being-chosen` is the name
# of a row whatever that row ends up saying.

# The month goes first. Every one of these four is the same date in the same
# order the rest of the English-speaking world writes back to front.
date-full = { $month } { $day }, { $year }
date-short = { $weekday } { $month } { $day }
date-numeric = { PAD2($month) }/{ PAD2($day) }/{ $year }
clock-corner = { $month }/{ $day } { $time }

# Colour.
shell-accent-color = Accent color
shell-color-temperature = Color temperature
shell-srgb-color-intensity = sRGB color intensity
shell-how-saturated-srgb-colour-is-made = How saturated sRGB color is made
shell-the-colour-of-being-chosen = The color of being chosen
shell-exactly-the-colour-sdr-showed = Exactly the color SDR showed
night-light-nested-session = Nothing here owns a color ramp: the session is running inside another compositor, which owns what its window is tinted with
hdr-no-display-or-pipeline = No connected display reports HDR, or the driver has no color pipeline to feed it
