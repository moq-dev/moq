# [S] A video-only broadcast's auto target stops creeping

## Goal

In `@moq/watch`, a broadcast with no audio played at `"auto"` delay settles
on a target measured from its real arrival timing, instead of creeping upward
one step at a time as it does today.

## Plan

Found while landing [#4162](https://github.com/moq-dev/moq/pull/4162), and
planned as a follow-up of the 2026-10-05 audit. `doc/concept/audio-jitter.md`
explains the mechanism: a subscription cut to the target hides every frame
later than the target, so the estimate creeps up one bucket at a time while
the frames it should have measured go unplayed. In automatic mode the browser
escapes that only through the audio subscription, which subscribes with at
least the 2 s ceiling. Browser video keeps the shared subscription budget,
because a longer one fetches whole stale groups, so a video-only broadcast
measures only the lateness that budget lets through.

- Reproduce first with a recorded or shaped video-only trace, and assert the
  target series, as the audio replay tests do.
- Find a way for video to observe late frames without fetching whole stale
  groups: for example, observe arrival at the container consumer before the
  local budget applies, as audio does, or a separate observation budget.
  Weigh the cost of fetching stale groups against a target that never
  converges.
- Update `doc/concept/audio-jitter.md` with whatever changes, since it
  documents today's limitation.

Public API: none expected. Wire: none.

## Related

- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - the estimator and the shared budget this works around
- [A/V clock](/quest/m1/av-clock.md) - the browser's sync between the tracks' targets
