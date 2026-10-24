# [S] Profile the io_uring workers

## Goal

`just bench-profile` profiles the `io-uring-workers` relay as well as tokio,
so every io_uring perf quest reads its CPU, allocation, and lock profile
from the same recipe instead of working around it.

## Plan

`run_profile` (`bench/run.sh`) builds without `moq-relay/io-uring` and
always runs `tokio-workers`. `just bench-runtime` already builds with the
feature, checks kernel support, and runs `io-uring-workers`; reuse that
build and check rather than adding a second path. Select the runtime by
argument, keep worker count and load identical across runtimes, keep
the existing capture modes, and fail the run if the relay did not start on
the selected runtime. Also take a base revision the way `just bench BASE`
does, building it in a worktree with the current harness, since a revision
older than this quest has no runtime selector; a before/after such as the
[clock profile](/quest/m2/perf/uring-clock-profile.md)'s then profiles the
ring at both.

Once it works, read these off a chat, audio, fanout, and mixed-traffic
profile, and plan a quest only for one that shows up:

- the per-receive `Vec` allocation for the usually single-entry fed list in
  `Endpoint::demux`;
- atomic RMWs in worker metrics (remote wake counters genuinely have
  several writers);
- receive rearm churn that `IORING_RECVSEND_POLL_FIRST` would cut;
- GSO batching on ordinary and jumbo paths, now that discovery can raise
  the MTU.

Decided 2026-10-22 from the io_uring audit.

Public API: none. Wire: none.
