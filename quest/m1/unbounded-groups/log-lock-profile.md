# [XS] Log lock profile

## Goal

`just bench-profile lock video-heavy` runs before and after the epoch-based
`kio::log` ([moq-dev/moq#6782](https://github.com/moq-dev/moq/pull/6782)) on a
quiet profiling host (see `bench/README.md`), and shows no lock-contention
regression: `GroupState`'s share of mutex wait with multi-frame groups drops
or holds.

## Plan

- Compare the commit before #6782 with the one that merged it, at each worker
  count, and post the wait-by-stack diff on the PR that finishes this. The
  recipe's base revision
  ([moq-dev/moq#6680](https://github.com/moq-dev/moq/pull/6680)) covers the
  "before" side in one run:
  `just bench-profile lock video-heavy "1 4 16" tokio-workers d854bf98b1~1`.
- Decided 2026-10-27 (maintainer): the profiling host is the 24-core
  `desktop` workstation (`kernel.perf_event_paranoid=-1`), run while quiet:
  no other agents running and load near zero. That retired the profiling-host
  condition this waited on.
- #6782's body holds the criterion numbers, taken on an oversubscribed host:
  `group_contended` costs 13 to 61% less per reader across cores, and about
  4 ns more per frame on one thread. A profile that contradicts them goes back
  to the maintainer.
- A 2026-10-30 attempt ran on `desktop` while other agents kept the 1-minute
  load between 5 and 30
  ([moq-dev/moq#7300](https://github.com/moq-dev/moq/pull/7300)). At 4 workers,
  `GroupState` held 1% of mutex wait before #6782 and none after it, but
  unprofiled 1-worker throughput swung from 11k to 30k fps between runs, and
  the 16-worker capture failed on perf dropping events or recording
  nonalternating switches. Profiling `d854bf98b1` as the base as well isolates
  #6782 from later changes.
- Decided 2026-10-30 (maintainer): keep this quest and rerun both bases on a
  quiet `desktop`; the noisy attempt does not finish it. On `main` (4 workers)
  that attempt also showed `GroupState` waits under
  `group::Consumer::poll_read_frame_once` → `Prefetch::pop_cached`, which
  #6782 alone did not. If they persist on the quiet rerun, suggest a follow-up
  quest profiling `d854bf98b1` against `main`.
- A 2026-11-03 rerun began on `desktop` at 0.21 one-minute load,
  with no other T3 agents running, and captured both historical bases at
  1/4/16 workers ([moq-dev/moq#7742](https://github.com/moq-dev/moq/pull/7742)).
  At 1 worker, both historical captures had zero blocked mutex wait, so
  their shares are undefined; this does not measure the per-frame pin cost.
  At 4 workers, `GroupState` wait fell from 8 us (0.50% of mutex wait) to
  none. At 16 workers, its share fell from 11.72% to 1.78%, but its absolute
  wait rose from 47 us to 599 us in `Consumer::poll_read_frame_once` ->
  `Cursor::poll_read_frame`. Total mutex wait rose from 0.401 ms to
  33.722 ms, so the share alone hides the contrary absolute result.
  Quietness was checked before the attempt, not recorded per capture, so
  these are candidate observations, not quiet-host proof. These single
  five-second captures do not establish a regression or its absence. The PR retains the reduced evidence and stack diff.
- The current source was [b5a2e8fb77](https://github.com/moq-dev/moq/commit/b5a2e8fb7716144abd756ae81cd9a780b200be0b),
  whose tree equals main at [801fd9a2f2](https://github.com/moq-dev/moq/commit/801fd9a2f2f5c88371fa833d805620d97d372465).
  Both 4-worker captures have group-state waits under
  `Consumer::poll_read_frame_once<..., Prefetch::pop_cached>` (26 and 18 us),
  with 937 us of total group-state wait at 16 workers in the first run. The second current 16-worker capture was
  refused for nonalternating switch records, so it supplies no measurement.
  Once a rerun establishes the quiet-host requirement, suggest the separate
  epoch-versus-main quest if these cached-read waits persist.

## Waiting

- Maintainer: resolve the contrary 16-worker absolute wait before calling this
  a no-regression result. Recommend repeating the two historical revisions in
  alternating order with warm direct-Nix-Cargo builds, recording host load
  per capture and verifying no other agents run throughout. Keep the
  workload, worker counts and capture validation unchanged. Alternatively, explicitly
  accept the share-only goal despite the increased absolute wait. No
  production change is justified by these single captures.
