# [S] Profile the io_uring workers

## Goal

`just bench-profile` profiles the `io-uring-workers` relay as well as tokio,
so every io_uring perf quest reads its CPU, allocation, and lock profile
from the same recipe instead of working around it.

## Plan

The harness now selects `tokio-workers` or `io-uring-workers` with the fourth
`bench-profile` argument and a base revision with the fifth. Both use the
runtime comparison's feature build and support check, and actual worker thread
names verify the selected backend and worker count before load. A base uses the
current harness, generator and sampler from a temporary worktree.

Remaining work needs a quiet Linux profiling host. CPU and lock captures refuse
`perf_event_paranoid=4`; configure the documented perf access on a suitable host
before measuring. Keep worker count and workload identical across runtimes and
revisions, and retain the unprofiled controls and capture evidence.

For example, `just bench-profile "cpu lock heap" chat 4 io-uring-workers`
profiles one shape. Also run `audio`, `fanout` and mixed traffic (`video`), and
compare Tokio at the same worker count. The optional base argument profiles both
revisions, including a base whose harness predates runtime selection.

Read these off the profiles, and plan a quest only for one that shows up:

- the per-receive `Vec` allocation for the usually single-entry fed list in
  `Endpoint::demux`;
- atomic RMWs in worker metrics (remote wake counters genuinely have
  several writers);
- receive rearm churn that `IORING_RECVSEND_POLL_FIRST` would cut;
- GSO batching on ordinary and jumbo paths, now that discovery can raise
  the MTU.

Decided 2026-10-22 from the io_uring audit.

Public API: none. Wire: none.
