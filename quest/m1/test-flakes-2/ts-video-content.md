# [S] TS compliance keeps every decoded picture

## Goal

CI's `TS compliance` step (`just test ts`, `.github/workflows/ts.yml`) stops
failing intermittently with check `video-content` ("decoded pictures are not
an intact suffix of the source", `test/ts/decode.py`), fixed at its cause.
The check stays as strict: a captured suffix that loses or duplicates a
picture still fails.

## Plan

Seen 2026-10-13 on #5471, #5481 and two unrelated branches within the same
hour; each passed on rerun. Both PRs have since merged and touch the path:
#5471 changed the TS export (`rs/moq-mux/src/container/ts/export.rs`) and
shared source and selection code, and #5481 changed `rs/moq-mux/src/clock.rs`
and the catalog clock. The unrelated branches failing too points to an older
cause, but check a failing capture against those diffs before ruling them
out.

This check guards real delivery, so treat it as a possible product bug first,
not a harness flake. Reproduce it by looping `just test ts` beside a CPU hog,
keep the source and capture from a failing run, and diff the picture digests
to see whether pictures are missing, duplicated, corrupted, or the suffix
start is misjudged. Then fix the side at fault: the import, relay, or export
losing or altering a picture, or the harness picking the wrong admitted
start. Never loosen the comparison, widen a timeout, or add a retry. Land a
regression that fails without the fix where one is easy.

Public API: none expected. Wire: none.
