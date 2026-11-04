# [S] The receive-windows benchmark runs to the end

## Goal

`moq-tokio`'s `benchmark_receive_windows`
(`rs/moq-tokio/tests/integration/receive_credit.rs:1056`) passes on `main`
instead of failing with `frame: Evicted`, so Nightly's `Rust benchmark
tests` job (`gh nightly rust-test`) passes and that night's report is
published. Fixed at the cause, never by loosening what the benchmark
measures.

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
  with the stock window. Local review rejected this change because it
  alters the measured pressure and excludes admission wait from frame
  latency. The implementation was removed; no benchmark or production
  code changes remain in this PR.
- Retain the stream/frame/window axes and actual offered workload for
  [Receive pool](/quest/m2/receive-pool.md). Resolve the policy below
  before changing either the benchmark's acceptance or production
  scheduling.
- Repro: `just rs test -p moq-tokio --test integration --release -E
  'test(benchmark_receive_windows)' --run-ignored only --success-output
  immediate`. Run expensive builds and sweeps through `quest gate`.

Public API: none. Wire: none.

## Waiting

- Maintainer: decide whether deliberately overcommitted cells should measure loss or remain lossless. This changes benchmark acceptance or production scheduling policy, which the implementation agent cannot select while the maintainer is AFK. (1) Recommended: preserve the offered workload, report delivered frames and evictions alongside throughput/latency, and retain zero-eviction assertions for workloads whose incomplete units fit. This requires revising the Goal's acceptance explicitly. (2) Keep every existing cell lossless at the current windows and change production sender admission/scheduling so an incomplete unit can finish before the window fills; measure the effect on equal-priority fairness and latency. Check this wait by recording the maintainer's pick in this Plan and the PR; automated reproductions and the rejected semaphore cannot decide that policy.
