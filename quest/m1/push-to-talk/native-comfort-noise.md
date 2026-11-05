# [M] Native playback fills declared silence with comfort noise

## Goal

`rs/moq-audio` playback, as used by `moq play`, treats a publisher's declared
silence the way `@moq/watch` does: when the ring runs dry at a declared end it
plays level-matched comfort noise instead of plain silence, counts no underrun
or stall, and keeps that silence through the next utterance's refill. A
push-to-talk listener on `moq play` then matches the browser.

## Waiting

- moq-dev/moq: [moq-dev/moq#7942](https://github.com/moq-dev/moq/pull/7942)
  merges. It defines how a suppressed span is declared and already gives
  `moq-audio`'s jitter estimator the declared-silence exception. Check that
  it is merged and `quest/m1/push-to-talk/dtx.md` is gone from `main`.

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

Build on [moq play reaches zero steady-state underruns by
time-stretching](/quest/m1/audio-jitter-target/native-stretch.md), whose
stretch in the same playback ring is on `main` since
[moq-dev/moq#7856](https://github.com/moq-dev/moq/pull/7856).

Public API: none expected. Wire: none.
