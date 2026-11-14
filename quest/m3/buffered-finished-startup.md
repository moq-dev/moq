# [S] Buffered startup releases on a finished track

## Goal

A `<moq-watch>` with a nonzero `lookahead` that joins a broadcast whose
publisher already finished, holding less media than the playout floor, plays
what it received and reads `offline` once played, instead of holding its
startup until the broadcast closes.

## Plan

Found while making a finished broadcast read `offline` once its tail played:
the decoders now report a clean track finish (`video.out.ended`,
`audio.out.ended`), but buffered startup in `js/watch/src/sync.ts`
(`#runStartup`) waits for every track's span to reach the delay, which a
finished track shorter than that never does. Reproduce it first with a test on
the controlled clock: a buffered player, a track that finishes 200 ms into a
1 s floor. Then let a finished track's span count as full, so startup releases
on what exists.
