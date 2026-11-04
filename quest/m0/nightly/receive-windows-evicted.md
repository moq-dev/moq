# [S] The overcommitted receive-window cells run again

## Goal

`moq-tokio`'s `benchmark_receive_windows`
(`rs/moq-tokio/tests/integration/receive_credit.rs`) measures its three
overcommitted cells again, and Nightly's `Rust benchmark tests` job
(`gh nightly rust-test`) still passes. Those cells are 256 streams,
4 MiB frames, and 4 MiB frames with a following stream window. Until
then, the benchmark skips them and prints each one with
`quest/m1/group-drops.md` as the reason.

Every cell keeps the workload it offers. A cell whose incomplete units
fit the connection window fails on any eviction. A cell that
overcommits the window reports its delivered frames and evicted streams
alongside throughput and latency, instead of failing.

## Plan

Decided 2026-11-03 (maintainer): m0, as a child of
[Nightly passes on main](/quest/m0/nightly/README.md).

Overload policy, decided 2026-11-04 (maintainer):

- ✅ Preserve the offered workload, report delivered frames and evictions
  alongside throughput and latency, and keep zero-eviction assertions only
  for workloads whose incomplete units fit.
- Not chosen: keep every cell lossless by changing production sender
  admission or scheduling.

Silent loss, decided 2026-11-04 (maintainer):

- ✅ Fix it at the source in `quest/m1/group-drops.md`, planned in
  [moq-dev/moq#7890](https://github.com/moq-dev/moq/pull/7890). There,
  the wire always names an abandoned group, and `track::Subscriber`'s
  drop signal becomes public.
- Not chosen: a benchmark give-up timeout that declares outstanding
  groups lost, or publisher-paced sending that returns in-flight credit
  once a group is sent.

Split, decided 2026-11-04 (maintainer):

- ✅ Land the cells that fit now, and skip the three overcommitted cells
  until group-drops lands. This temporarily revises the original Goal's
  rule against loosening what the benchmark measures, and the revision
  lasts until those cells run again.
- Not chosen: keep the whole benchmark waiting on group-drops.

Landed in [moq-dev/moq#7838](https://github.com/moq-dev/moq/pull/7838):

- `Cell::fits`: a cell's incomplete units fit when `streams × min(frame,
  1 MiB storage segment)` is at most the connection window.
  Equal-priority tracks interleave by packet, so each can hold one frame,
  or one segment of a larger frame, before any completes. The default,
  1-stream, 16 KiB-frame, and 8/32 MiB-window cells fit. They pass on
  both paths with every frame delivered and nothing evicted.
- The CSV adds `delivered_frames` and `evicted_streams`.
  `sh/gh/bench-report.py` reads them by exact name as measurements, not
  dimensions.
- `measure` already counts an `Evicted` group as lost when a cell
  doesn't fit, and it asserts zero evictions only for cells that fit.
- [moq-dev/moq#7729](https://github.com/moq-dev/moq/pull/7729)'s zero
  default max age expired the benchmark's superseded in-flight groups at
  the publisher. The benchmark's tracks now set no max age.

What remains:

- Each track paces itself on the groups its reader resolves. The
  subscriber also abandons some groups without `recv_group` ever
  returning them: a stream evicted before its group header was
  identified, or an evicted group whose refill FETCH was evicted too
  (traced as `fetch abandoned`). Such a group's in-flight permit never
  returns, so the writer stalls and the reader times out. The 4 MiB-frame
  cell hit this on every run, and the 256-stream cell intermittently.
- Once group-drops lands, the reader returns a dropped group's permit and
  counts the group as lost. Then remove the skip (`OVERCOMMITTED` and the
  `Cell::fits` partition in `benchmark_receive_windows`), and run the full
  sweep through `quest gate`.
- Retain the stream/frame/window axes and the actual offered workload
  for [Receive pool](/quest/m2/receive-pool.md).
- Repro: `just rs test -p moq-tokio --test integration --release -E
  'test(benchmark_receive_windows)' --run-ignored only --success-output
  immediate`.

Investigation history: the regression boundary at
[moq-dev/moq#7624](https://github.com/moq-dev/moq/pull/7624), where the
stock connection window shrank from 128 MiB to 4 MiB, and the rejected
connection-wide admission semaphore are recorded in
[moq-dev/moq#7838](https://github.com/moq-dev/moq/pull/7838).

Public API: none. Wire: none.

## Waiting

- Outside condition: `quest/m1/group-drops.md`, planned in
  [moq-dev/moq#7890](https://github.com/moq-dev/moq/pull/7890), finishes,
  making `track::Subscriber`'s drop signal public on `main`. Check that
  [moq-dev/moq#7890](https://github.com/moq-dev/moq/pull/7890) has
  merged and that the quest file has since been deleted from `main`.
