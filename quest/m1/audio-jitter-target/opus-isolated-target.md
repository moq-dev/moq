# [S] The isolated ring holds the target at near-zero jitter

## Goal

On the `near-zero` network profile, Opus on the isolated (SharedArrayBuffer)
ring settles at the same playout target as the postMessage ring, with no
underrun episodes, in the nightly audio-quality rows.

## Plan

Measured by [#5442](https://github.com/moq-dev/moq/pull/5442) over the 7
scheduled nightlies of 2026-10-07 to 2026-10-13: Opus near-zero on the
isolated ring reached a `target_ms_p95` of 100 to 800 ms with underrun
episodes of about 1 s, while the postMessage ring on the same profile held at
60 ms. A network with almost no jitter should not grow the target, so the
first suspect is the isolated page's path rather than the estimator.

Start after #5442 merges: it uploads the nightly artifacts and carries the
measured budgets this quest tightens.

Decided 2026-10-13 (maintainer): a child of the audio jitter target line,
reproducible from the harness now, so Watch's manual proof runs after it.

Reproduce it first, from the nightly artifacts and then in
`js/watch/src/audio/replay.ts` or a ring-level test on a simulated clock.
The target and the underruns may have separate causes. The target follows
`consumer.spread`, which `js/hang/src/container/consumer.ts` records from
transport arrivals before the ring, so a ring defect cannot inflate it:
compare the captured arrivals and configuration of the two rows first, for
something on the cross-origin isolated page that delays when frames are
observed. A ring that under-reports its fill, reads the worklet's position
late, or misses a resize explains underruns only. Fix each at the source with
a regression that fails without it, then tighten the isolated near-zero
ceilings in `test/audio-quality/budgets.json` from re-measured nightly runs.

Public API: none expected. Wire: none.

## Related

- [Watch](/quest/m1/audio-jitter-target/watch.md) - the manual browser proof that follows this fix
- [AAC bursty re-stall](/quest/m1/audio-jitter-target/aac-bursty-restall.md) - the other defect the same measurement found
