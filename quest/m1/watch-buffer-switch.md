# [S] Switching buffer at runtime switches the audio ring

## Goal

Changing a `<moq-watch>`'s `buffer` after playback started switches the
audio ring's mode: going from live to buffered stops shedding future-dated
audio, and going back leaves no live player in play-through.

## Plan

Found by the 2026-10-24 pre-cut audit. Decided 2026-10-27 (maintainer): its
own ready quest, split out of
[buffered ascending](/quest/m1/buffered-ascending.md), which waits on
lite-07 while this bug does not.

`js/watch/src/audio/decoder.ts` reads `sync.out.buffered` once when it builds
the worklet's ring (about line 294), and `#order()` peeks it once per
attempt, so going from live to buffered after playback started leaves
`Backpressure` and the worklet `Init` live (future-dated TTS audio is shed),
and the reverse leaves a live player in play-through. Key the ring and
`Order` on `sync.out.buffered`: rebuild the ring, or post a mode message plus
a reset. Test both directions on a running player.

Public API: none. Wire: none.
