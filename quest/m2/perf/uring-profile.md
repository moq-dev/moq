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
argument, keep worker count and load identical across runtimes, and keep
the existing capture modes. Comparing revisions stays running the recipe at
each one on the same host.

Once it works, read these off a chat, audio, fanout, and mixed-traffic
profile, and plan a quest only for one that shows up:

- the per-receive `Vec` allocation for the usually single-entry fed list in
  `Endpoint::demux`;
- atomic RMWs in worker metrics (remote wake counters genuinely have
  several writers);
- receive rearm churn that `IORING_RECVSEND_POLL_FIRST` would cut;
- GSO batching on ordinary and jumbo paths after
  [MTU parity](/quest/m1/uring-mtu.md).

Decided 2026-10-22 from the io_uring audit.

Public API: none. Wire: none.
