# [XS] Muted and volume are independent on the watch element

## Goal

`<moq-watch volume="0.8" muted>` starts muted, and unmuting plays at 0.8, in
either attribute or property order. Muting no longer rewrites `volume`.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Reproduced, inherited.

Two effects in `js/watch/src/element.ts:259-273` write each other's signal
in the same flush: setting `volume` then `muted` in one tick ends unmuted at
0.5, losing both. Delete the coupling; the `Emitter` already takes `muted`
and `volume` separately, so apply `muted ? 0 : volume` at the gain node. A
test sets both in each order in one tick.

Public API: behavior change (muting no longer zeroes the `volume`
attribute or property, and a zero volume no longer reports as muted); worth
making before the cut. Wire: none.
