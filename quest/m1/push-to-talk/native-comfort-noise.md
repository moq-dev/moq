# [M] Native playback fills declared silence with comfort noise

## Goal

`rs/moq-audio` playback, as used by `moq play`, treats a publisher's declared
silence the way `@moq/watch` does: when the ring runs dry at a declared end it
plays level-matched comfort noise instead of plain silence, counts no underrun
or stall, and keeps that silence through the next utterance's refill. A
push-to-talk listener on `moq play` then matches the browser.

## Plan

Planned 2026-11-05 (maintainer, /quest-plan of the follow-ups from the
2026-11-05 /quest-complete run), from #7942, whose agent left native playback
with the estimator exception but no comfort noise because its ring has no
notion of a declared end. Milestone: ✅ m1.

Follow the browser's choices in #7942 unless measurement argues otherwise: the
decoder hands the ring each declared end less the Opus pre-skip, and comfort
noise is white noise matched to the quietest 20 ms block played, capped at
-60 dBFS (`js/watch/src/audio/comfort.ts`). Share the level rule with the
browser through `doc/concept/audio-jitter.md` and a corpus case rather than
two descriptions. Cover it in `test/audio-quality`'s native rows or
`just test push-to-talk` if that can drive `moq play`.

Coordinate with [moq play reaches zero steady-state underruns by
time-stretching](/quest/m1/audio-jitter-target/native-stretch.md), which edits
the same playback ring: whichever lands second rebases onto the other.

Public API: none expected. Wire: none.
