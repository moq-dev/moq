# [M] js/watch: the playout target follows the spec, proven in a real browser

## Goal

`js/watch` computes its auto target with the algorithm from
[Spec](/quest/m0/audio-jitter-target/spec.md) and passes the conformance
vector. The "Real-time" preset plays clean audio on a LAN and against the
public relay: zero underruns after convergence and no skip-aheads in steady
state, on both the isolated and the postMessage ring paths, confirmed by a
manual run on Chrome and Safari with a real microphone and 40 ms or more of
added RTT.

Boundaries: the ring's slack, re-stall and underrun counter are mechanical and
already correct in #3517; this quest changes the estimator above them.

## Plan

Fix PR #3517 in place on `dev` rather than reopening it. It is a published
`@moq/watch` break (the `Sync` input `probe` is removed) and `dev` already
carries the `delay`/`buffer` split it is expressed in. Resolve its conflicts
with `dev` first, since it currently does not merge.

- Replace the estimator in `js/hang/src/container/jitter.ts` with the spec's,
  and run the conformance corpus in `jitter.test.ts`. Keep the arrival
  plumbing, the `spread` getter, and the measurement point: those were right.
- The reproduced 14.56 s runaway is the regression test to write first, before
  any replacement: two frames whose media timestamps are far apart, then paced
  audio with zero real jitter, asserting the target stays near the frame
  duration. On today's branch it reads 14.5 s and needs about fifteen minutes
  of clean audio to unwind. Take the frame duration from the rendition config
  rather than from observed timestamps, clamp the target, and bound how fast it
  rises; [Spec](/quest/m0/audio-jitter-target/spec.md) has the detail.
- Check what the viewer's saved preset does on load. The element defaults
  `delay` to `"auto"` and nothing in `demo/` overrides it, yet a fresh session
  came up on the 100 ms chip, so something is restoring or overriding it. Pin
  that down: a stored preference silently winning over the default is its own
  bug, and it also means auto gets far less real exposure than it looks like.
- Keep the ring work as it stands: one chunk of slack above the target, landing
  back on the target when skipping, re-stalling on empty, and the underrun
  counter reaching the stats panel and the buffering indicator.
- Replay the recorded traces from #3477 rather than synthetic ones of the same
  shape. They are on the reporter's fork
  (`fperex/moq`, branch `debug/rt-audio`) with the raw ndjson attached to
  release `rt-audio-traces-2026-09-06`. Trim a copy into the repository and
  replay it through both rings in `replay.test.ts`.
- Manual run against the public relay on Chrome and Safari, the two rows the
  issue measured. Measure the publisher's audio encoder input-to-output lag in
  the same run using the reporter's instrumented harness; #3518 fixed the known
  cause, so the 7.35 s lag and the 88 to 275 ms/s drift the issue reported
  stand unconfirmed. If drift survives, the suspects are `writeFrame` opening a
  group per audio frame under WebTransport stream credit and the main-thread
  task queue delivering encoder output. Turn that into its own quest rather
  than fixing it here.

Two sibling quests on `main` touch the same audio path
(`3479-watch-audio-identity`, `publish-audio-encoder-lag`), for conflict triage
when `main` next merges into `dev`.

## Required

- [Spec](/quest/m0/audio-jitter-target/spec.md) - the algorithm this implements
