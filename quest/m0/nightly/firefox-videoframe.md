# [S] Firefox's player closes every VideoFrame

## Goal

Nightly's Media job no longer fails Firefox's player on the console error "A
VideoFrame was garbage collected without being closed".

## Plan

Reported in Nightly [40506938673](https://github.com/moq-dev/moq/actions/runs/40506938673) (2026-10-27, at 22808bbb2f).
The archived Media log labels this warning `[fixture]`, the Chromium publisher,
not `[player]`. Confirm the emitting owner before claiming a Firefox player fix.
[Firefox publishing](/quest/m2/publish-firefox.md) covers the Firefox publisher only.
Keep the console failure loud.

The current attempt accounts for decoded originals and every clone. Injecting
output from a cancelled codec reproduces an unclosed preview clone: the callback
created it before checking cancellation, after the track had released its poster.
The candidate fix binds output to the codec lifetime and refuses cancelled output
before retaining anything. Deterministic legacy/CMAF tests cover replacement,
disable, failure, close, skip, and the resolved-wait teardown race.

Remaining: reproduce the native browser warning and identify the fixture's lost
frame. Confirm that native cancelled decoder output can reach the candidate guard;
otherwise drop that guard and fix the demonstrated owner. This attempt has not
established the cause of the archived warning.

Local watch tests (424), type checking, build/declaration emit and Biome passed.
`quest gate just ci` and the Firefox media lane could not start because local
admission remained blocked; they were cancelled before publication. Run those
checks, plus the real development player, before completing this quest.

Public API: none. Wire: none.
