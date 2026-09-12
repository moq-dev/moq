# [M] js/watch: the playout target follows the spec, proven in a real browser

## Goal

`js/watch` computes its auto target with the algorithm from
[Spec](/quest/m2/audio-jitter-target/spec.md) and passes the conformance
vector. The "Real-time" preset plays clean audio on a LAN and against the
public relay: zero underruns after convergence and no skip-aheads in steady
state, on both the isolated and the postMessage ring paths, confirmed by a
manual run on Chrome and Safari with a real microphone and 40 ms or more of
added RTT.

Boundaries: the ring's slack, re-stall and underrun counter are mechanical and
already correct on the #3517 branch; this quest changes the estimator above
them.

## Plan

Start from `origin/quest/m0/3477-watch-auto-latency`, the branch of the closed
PR #3517, two commits ahead of `dev`. Rebase it onto the base branch first.
Nothing on `main` or `dev` has any of it: `js/watch/src/sync.ts:150` still
sizes auto from `minRtt * 1.25`.

What the branch already has, and this quest keeps:

- The RTT term is gone. `js/watch/src/audio/latency.ts` is deleted, and
  `MIN_JITTER`, `FALLBACK_JITTER` and `#minRtt` are gone from `sync.ts`; auto
  is the larger of the per-track spreads.
- The measurement point: `Container.Consumer` observes each frame at arrival,
  before the age budget can skip a group, and exposes it as a `spread` getter.
- The estimator in `js/hang/src/container/jitter.ts`: a histogram of 5 ms
  buckets, 200 of them, decayed per arrival by `0.9993`, read at the 95th
  percentile and clamped to `BUCKETS * BUCKET` (one second) and to the largest
  spread seen; the arrival minimum expires over two 30 s windows; `reanchor()`
  drops the baseline on a discontinuity; the step down is bounded to one frame
  per second. `jitter.test.ts` covers it.
- The ring work: one chunk of slack above the target, landing back on the
  target when skipping, re-stalling on empty, and the underrun counter reaching
  the stats panel and the buffering indicator. `replay.test.ts` drives both
  rings from a recorded arrival trace.

What remains is conformance to the spec:

- The "plus one frame" term. `jitter.ts` learns it as the running minimum of
  positive gaps between consecutive observed timestamps, so the first gap sets
  it with nothing to validate against, `#publish` raises the target to
  `percentile + step` immediately, and `reanchor()` clears `#latest` but not the
  learned spacing. That is the 14.56 s reading reproduced in the spec, and it
  still reproduces on the branch head. Write the regression first: two frames
  whose media timestamps are far apart, then paced audio with zero real jitter,
  asserting the target stays near the frame duration. Then take the frame
  duration from the source the spec settles on rather than from observed
  timestamps, and bound the rise.
- The forget factor: the spec's resampled 500 ms maximum with the underrun
  factor, decayed per observation rather than per arrival, replacing the
  per-arrival `0.9993`.
- Reordered arrivals excluded from the reference and the histogram, per the
  spec.
- Run the conformance corpus in `jitter.test.ts` alongside the existing cases.
- Check what the viewer's saved preset does on load. The element defaults
  `delay` to `"auto"` and nothing in `demo/` overrides it, yet a fresh session
  came up on the 100 ms chip, so something is restoring or overriding it. Pin
  that down: a stored preference silently winning over the default is its own
  bug, and it also means auto gets far less real exposure than it looks like.
- Replay the recorded traces from #3477 rather than synthetic ones of the same
  shape. They are on the reporter's fork (`fperex/moq`, branch
  `debug/rt-audio`) with the raw ndjson attached to release
  `rt-audio-traces-2026-09-06`. Trim a copy into the repository and replay it
  through both rings in `replay.test.ts`.
- Manual run against the public relay on Chrome and Safari, the two rows the
  issue measured. Measure the publisher's audio encoder input-to-output lag in
  the same run using the reporter's instrumented harness; #3518 fixed the known
  cause, so the 7.35 s lag and the 88 to 275 ms/s drift the issue reported
  stand unconfirmed. If drift survives, the suspects are `writeFrame` opening a
  group per audio frame under WebTransport stream credit and the main-thread
  task queue delivering encoder output. Turn that into its own quest rather
  than fixing it here.

The branch also replaces `probe` in `SyncInput` with per-track `audioSpread`
and `videoSpread` inputs, which breaks the published `@moq/watch` type. This
quest lands on `main`, so it adds the spread inputs beside `probe` and stops
reading `probe`; removing it is part of the `SyncInput` reshape in
[Plan: A/V clock](/quest/m1/plan-av-clock.md) on `dev`. Land the estimator so
that quest can adopt it without a second estimator change.

## Required

- [Spec](/quest/m2/audio-jitter-target/spec.md) - the algorithm this implements

## Related

- [Plan: A/V clock](/quest/m1/plan-av-clock.md) - reshapes `SyncInput` around the per-track spread this quest produces
