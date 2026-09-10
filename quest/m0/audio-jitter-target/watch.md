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

- Replace the 95th-percentile-over-decaying-histogram estimator in
  `js/hang/src/container/jitter.ts` with the spec's, and run the conformance
  vector in `jitter.test.ts`. Keep the arrival plumbing, the `spread` getter,
  and the measurement point: those were right.
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
