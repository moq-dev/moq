# [S] The receive-windows benchmark runs to the end

## Goal

`moq-tokio`'s `benchmark_receive_windows`
(`rs/moq-tokio/tests/integration/receive_credit.rs`) passes on `main`
instead of failing with `frame: Evicted`, so Nightly's `Rust benchmark
tests` job (`gh nightly rust-test`) passes and that night's report is
published. Every cell keeps the workload it offers. A cell whose
incomplete units fit the connection window still fails on any eviction.
A cell that overcommits the window reports its delivered frames and
evicted streams alongside throughput and latency, instead of failing.

## Plan

Decided 2026-11-03 (maintainer): m0, as a child of
[Nightly passes on main](/quest/m0/nightly/README.md).

Investigated 2026-11-04 in
[moq-dev/moq#7838](https://github.com/moq-dev/moq/pull/7838):

- Unchanged main reproduces `frame: Evicted`. Both the merge of
  [moq-dev/moq#7597](https://github.com/moq-dev/moq/pull/7597) and its
  parent fail too. The subscription already asks for a 10-second
  `delay.max`; the Delay API migration did not introduce this failure.
- [moq-dev/moq#7624](https://github.com/moq-dev/moq/pull/7624) reduced
  the stock connection receive window from 128 MiB to 4 MiB. The harness
  still permits 1 MiB per track, or one larger frame, across up to 256
  tracks. The sender interleaves equal-priority tracks by packet, so
  incomplete storage segments can exhaust the connection window before
  any can seal. Eviction then frees the stuck credit by design.
  The unchanged sweep passes all 16 rows at its
  [parent](https://github.com/moq-dev/moq/commit/097dfc42257fc893dee5116006a265660fd71b01),
  then fails `frame: Evicted` in the 4 MiB frame cell at its
  [merge](https://github.com/moq-dev/moq/commit/cc3c1961d849dd536dc203724bf6b6cca46f0d88).
- A connection-wide semaphore makes all 16 rows pass, three runs each,
  and Nightly's report parser produces 80 measurements. It also admits
  only 15 tracks' 256 KiB frames, or three tracks' 4 MiB frames, at once
  with the stock window. A connection-wide admission semaphore is
  rejected: it changes the measured pressure and leaves admission wait
  out of frame latency.
- Retain the stream/frame/window axes and actual offered workload for
  [Receive pool](/quest/m2/receive-pool.md). Resolve the policy below
  before changing either the benchmark's acceptance or production
  scheduling.
- Repro: `just rs test -p moq-tokio --test integration --release -E
  'test(benchmark_receive_windows)' --run-ignored only --success-output
  immediate`. Run expensive builds and sweeps through `quest gate`.

Overload policy, decided 2026-11-04 (maintainer):

- ✅ Preserve the offered workload, report delivered frames and evictions
  alongside throughput and latency, and keep zero-eviction assertions only
  for workloads whose incomplete units fit.
- Not chosen: keep every cell lossless by changing production sender
  admission or scheduling.

Implemented in [moq-dev/moq#7838](https://github.com/moq-dev/moq/pull/7838):

- A cell's incomplete units fit when `streams × min(frame, 1 MiB storage
  segment)` is at most the connection window. That holds for the default,
  1-stream, 16 KiB-frame, and 8/32 MiB-window cells. All five pass on
  both paths with every frame delivered and nothing evicted. The 256-stream,
  4 MiB-frame, and follow cells overcommit.
- The CSV adds `delivered_frames` and `evicted_streams`.
  `sh/gh/bench-report.py` reads them as measurements, not as dimensions
  in the series name.
- [moq-dev/moq#7729](https://github.com/moq-dev/moq/pull/7729) made
  authored tracks default to a zero max age. The benchmark's tracks keep
  up to 1 MiB of groups in flight, so their superseded groups expired at
  the publisher and never arrived. Even the default cell timed out. The
  benchmark's tracks now set no max age, as before #7729.

Overcommitted cells still hang. A track paces itself on groups its reader
resolves, either delivered or `Evicted`. The subscriber also gives up on
some groups without `recv_group` ever returning them: a stream evicted
before its group header was identified, or an evicted group whose refill
FETCH was evicted too ("fetch abandoned"). No public API reports such a
group, so its in-flight permit never returns, the track's writer stalls,
and the reader times out. The 4 MiB-frame cell hits this on every run;
the 256-stream cell intermittently.

Public API: none so far. Wire: none.

## Waiting

- Maintainer: decide how the benchmark accounts for a group the
  subscriber silently gives up on, so overcommitted cells finish at
  their offered load. Options are in the PR's `## Decisions needed`.
  (1) Recommended: moq-net surfaces the loss. `recv_group` hands out a
  given-up group whose reads fail `Evicted`, or the subscriber counts
  given-up groups. This is a public behavior change. Production readers
  then learn of a loss as they already do for an identified evicted
  group. (2) Benchmark-only: declare a track's outstanding group lost
  after the subscriber's give-up interval. This is time-based, and the
  wait inflates elapsed time, deflating throughput. (3) Benchmark-only: release
  in-flight credit once the publisher finishes sending a group, not once
  the reader resolves it. This changes the offered-load model. Check
  this wait by recording the pick here and in the PR.
