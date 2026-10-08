# [S] @moq/publish never reuses catalog group numbers under one epoch

## Goal

`@moq/publish` never publishes different content under one name and epoch.
After an unannounce and re-announce within one run, its catalog track either
continues its group sequence or the broadcast goes out under a fresh epoch, so
a viewer or relay never mistakes a restarted catalog for the old one.

## Plan

Found by #4970 (broadcast-epoch apps): `@moq/publish` re-creates its catalog
track after an unannounce and re-announce, so catalog group numbers may
restart under the same name and epoch. That breaks the rule that a broadcast,
track, or group name always means the same content.

Decided 2026-10-07: verify it first with a test that unannounces and
re-announces a running publish and reads the catalog's group sequence. Fix it
only if it reproduces: continue the sequence, or mint a fresh epoch per
announce, whichever matches how the rest of `@moq/publish` treats a
re-announce. `js/publish/src/broadcast.ts` mints the epoch once and keeps the
publisher across unannounce and announce on purpose, which points at
continuing the sequence.

#4929 (merged) made an on-demand producer continue a name's sequences
across replacements in both languages, but JS `createTrack` and
`insertTrack` still start at 0 (`doc/lib/js/net.md`). If this reproduces,
consider fixing it there, in `@moq/net`'s `createTrack`, so every JS
publisher continues the sequence, rather than in `@moq/publish` alone.

Public API: none expected. Wire: none.

