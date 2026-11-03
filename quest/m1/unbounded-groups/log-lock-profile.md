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
- Decided 2026-11-03 (maintainer): repeat both historical revisions in
  alternating order on a quiet window, with warm direct-Nix-Cargo builds,
  per-capture host load, and no other agents running throughout, keeping the
  workload, worker counts and capture validation. `desktop` was busy, so the
  repeat used `nixos2` (i7-8700, 6 cores / 12 threads, no hybrid pinning,
  `perf_event_paranoid=-1`), with a per-capture monitor of load, CPU used
  outside the run, and other agents' tool commands.
- That 2026-11-03 repeat found no quiet window. Of three runs, only one
  pre-epoch run (`d854bf98b1~1`) completed its historical captures. Other
  agents ran light tool commands during it, using 0.06 to 0.21 cores on
  average (peak 1.0). In it, the 16-worker capture had 709 us of `GroupState`
  wait (1.83% of 38.8 ms), all under `Consumer::poll_read_frame_once` ->
  `poll_read_frame` -> `poll<GroupState>`. That is the stack behind the
  epoch capture's 599 us on `desktop`, so that wait predates #6782, and the
  `desktop` contrast (47 us before, 599 us after) is within one revision's
  capture-to-capture spread. At 4 workers it had 9 us (1.10% of 0.82 ms). Its
  current 4-worker capture had no `GroupState` wait; its current 16-worker
  capture was refused for nonalternating switch records. Two other runs are
  void: another session's moq bench builds used 5 to 6 cores throughout the
  epoch run, whose 16-worker capture perf also refused for a dropped event,
  and the first pre-epoch run overlapped another agent's builds. At 1 worker,
  the relay on this CPU delivers only 20 to 23k of the 31k offered frames per
  second (about 35% lifetime loss), so 1-worker captures on `nixos2` fail the
  zero-loss check; they had no mutex wait. No valid epoch capture exists on
  `nixos2`, so the share comparison has no same-host pair yet.

## Waiting

- Maintainer: a quiet benchmark window. `nixos2` and `desktop` both host
  agent sessions and other sessions' benchmarks, so no window stayed free of
  them for the roughly 45 minutes that two alternating pairs need (about
  11 minutes per run, including its 5-minute base rebuild). Recommend
  reserving one x86 host with `perf_event_paranoid=-1`, launching no agents
  or benchmarks there for that time, then repeating the alternating pairs.
  Alternatively, accept "no regression attributable to #6782" from the
  pre-epoch replicate showing the same stack at a similar absolute wait.
  No production change is justified by these captures.
