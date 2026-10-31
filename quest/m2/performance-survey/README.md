# Performance survey follow-ups

## Goal

Validate the performance risks found in the 2026-10-22 source audit with
benchmarks, then remove costs that matter to real consumers. A measured
no-win closes a quest with evidence; a suspicious loop alone does not justify
an optimization. Each child is independently landable.

This epic's remaining work is to reconcile the children's measurements into
the recurring performance audit: what mattered, what did not, and which
nightly series now protect the relevant workload.

## Plan

Requested on 2026-10-22: an extensive performance audit and quests requiring
benchmarks to validate findings. Audited main
[`1d43c096e044`](https://github.com/moq-dev/moq/commit/1d43c096e04431bfc87d807ce47a546df8017475).
These are source-supported hypotheses, not new measured regressions or
promised speedups. General optimization stays in m2 under the existing
milestone policy. Do not change API, wire, scheduling policy, or retention
contracts just to improve a number; bring a necessary contract change to the
maintainer with the measurement first. No new user guide is needed for this
planning change; implementations update documentation they make stale.

The follow-up audit on main
[`d44813cb703b`](https://github.com/moq-dev/moq/commit/d44813cb703bf0d5366266ae8206f794856c69f8)
adds memory lifetime, idle/churn scaling, kernel mechanisms and settings.
These additional findings are source-supported; no new memory or kernel
benchmark result is claimed.

### Benchmark contract for every child

- Reproduce the suspected cost in the production path before changing it.
  Sweep the dimensions named in the child independently, including both
  publisher and subscriber axes for fan-out. Keep useful delivered work
  comparable and verify payloads, ordering, lifecycle, and timing semantics.
- Retain baseline and candidate revisions, exact commands, raw samples,
  toolchain, machine, load, and build configuration. Use paired alternating
  runs and an A/A control to distinguish a gain from measurement noise.
  Report absolute cost and normalized cost, not only percentage changes.
- Attribute CPU work with profiles or instruction/operation counts; measure
  allocations, copied bytes, retained memory, and tail latency where relevant.
  Instruction counts do not measure blocking or prove end-to-end throughput.
  Setup and teardown boundaries must match between variants.
- For memory, include cold start, burst, quiet, repeat burst and teardown.
  Separate logical cache/flow-control charges, live allocations, allocator
  active/resident pages, virtual retained mappings and kernel socket memory.
  RSS alone cannot establish a leak or identify its owner. Record allocator,
  profiling mode and cgroup memory pressure. Only investigate allocator decay,
  arenas or background purging after application-owned retention is attributed;
  compare reclaim latency and CPU/fault cost on reuse. Reuse the phases in
  [heap profiling](/bench/profile.py). The
  [jemalloc statistics definitions](https://jemalloc.net/jemalloc.3.html)
  distinguish allocated, active, resident and retained memory.
- Measure JS changes in a real browser with the existing browser harness.
  Bun operation counts can demonstrate an algorithmic slope but cannot stand
  in for browser latency, GC, native copies, or hardware behavior.
- Network performance uses a rate below offered media bitrate behind a deep,
  finite queue whose overflow also drops packets. Random loss is useful for
  correctness only. Report delivery and latency beside CPU so doing less work
  cannot look like an improvement.
- Extend the existing benchmark harnesses and schedule each new benchmark
  at least nightly. Keep timing out of the merge gate. Fast deterministic
  correctness/complexity checks may gate; no arbitrary speedup threshold.
  Link the nightly command and retained report in the implementation PR.
- Land an optimization only when the measured consumer benefit justifies its
  complexity and memory tradeoffs. Otherwise retain the useful benchmark and
  record why the implementation was left alone.

For settings sweeps, record effective values, not only requested ones.
[Resolved client defaults](/rs/moq-tokio/src/quic.rs) currently allow 10,000
streams per direction, 128 MiB connection receive credit, 4 MiB per-stream
credit and 16 MiB send credit. [Relay peer overrides](/rs/moq-relay/src/limits.rs)
allow 100,000 streams and 640 MiB each of connection receive and send credit.
Standalone backend defaults differ again; benchmark the effective settings
of the actual consumer. These are limits, not per-connection allocations.
Include idle connections and stalled
readers as well as high-bandwidth, high-RTT relay links before recommending
smaller windows. Receive leases ([moq-dev/moq#5878](https://github.com/moq-dev/moq/pull/5878)) own budget
correctness; settings must preserve useful bandwidth-delay product and
failover behavior. The keep-alive already derives from the negotiated idle
timeout, so leave timer policy alone. Do not add a generic tuning surface without a measured consumer.

Reuse [the benchmark harness](/bench/README.md). Criterion's
[timing-loop guidance](https://bheisler.github.io/criterion.rs/book/user_guide/timing_loops.html)
explains setup/drop boundaries; the
[WebCodecs resource model](https://www.w3.org/TR/webcodecs/#resources)
means removing a JS copy still requires measuring native resource lifetime.
Neither source establishes a bottleneck in this repository.

### Evidence and coverage

The audit inspected Rust model/runtime/QUIC paths, JS transport/playback/
publish paths, media import/export, auth, storage, bindings and benchmark
orchestration. It did not run the full release comparison, real-browser
performance suite, GPU/platform codecs, or every generated wrapper. Those
remain with their existing owners. The recurring audit is not completed by
this source survey.

An operation-count probe imported the actual JS `bandwidth.allocate`, counted
`Want.priority` getter reads, and called it for every reservation, as
`Allocator.#publish` does. With one tier, 16/64/256 reservations caused
512/8,192/131,072 reads, exactly 2N². With distinct tiers, 16/64 caused
6,952/438,944 reads. Nix Bun 1.4.2 was used; no browser speedup is claimed.
The bandwidth child must retain a reproducible benchmark, including the
complete allocator and Rust consumers, before selecting a fix.

`python3 sh/gh/nightly.test.py NightlyTest.test_every_cargo_bench_target_runs_nightly`
reproduced the missing `moq-archive/producer` selection. The existing
[CI audit PR](https://github.com/moq-dev/moq/pull/6266) already fixes it.
No duplicate quest or workflow edit belongs here.

### Kernel and placement assessment

- Already implemented in [moq-uring](/rs/moq-uring/README.md): multishot
  receive, provided buffers, GRO/GSO, deferred/cooperative task running,
  coalesced futex wakes and per-worker CID-steered sockets. Measure these
  with existing ablations before adding another mechanism.
- [Ring submission setup](/quest/m2/3199-moq-uring-remove-sq-indirection-and-per-enter-ring-fd.md)
  and [completion batching](/quest/m2/3200-moq-uring-batch-completion-wakeups-with-min-timeout.md)
  already own the small ring experiments. Batching must preserve deadlines
  and latency tails, not merely reduce syscalls.
- [Zero-copy sends](/quest/m3/3201-moq-uring-use-sendmsg-zc-for-large-udp-gso-trains.md)
  need physical-NIC evidence: the previous loopback prototype lost 6-9%
  while exercising copy fallback. Deferred completion ownership, pinned
  memory and CQ pressure are costs to measure, not free benefits.
  [QUIC I/O ownership](/quest/m2/quic-io-boundary.md) owns the buffer seam.
- Extend [worker defaults](/quest/m2/perf/shard-default.md) with NUMA, SMT,
  NIC queues and CPU quotas. Application CID steering does not establish
  matching NIC/softirq locality; see the Linux
  [network scaling guide](https://docs.kernel.org/networking/scaling.html).
- Linux [io_uring zero-copy receive](https://docs.kernel.org/networking/iou-zcrx.html)
  describes a TCP receive path with hardware header/data split and dedicated
  queues. It is not evidence of a drop-in optimization for this UDP path.
  Confirm transport, hardware and minimum-kernel support before a future trial.
- NAPI busy polling and registered wait arguments were explicitly dropped in
  [Perf](/quest/m2/perf/README.md). Do not reopen them without new profiling
  evidence. SQPOLL, huge pages and blanket page locking likewise have no
  demonstrated need here; they can exchange CPU or reclaimable memory for
  lower latency. Incremental provided-buffer consumption is deliberately
  unsuitable for the current multishot `recvmsg` tail behavior.

### Existing ownership and deliberately deferred work

- [Release performance](/quest/m1/release-perf/README.md) owns the main
  against release comparison for this cut, including the sparse read slope;
  it already fixed or accepted resume duplicates, idle GC, stats drain, FETCH
  fan-out, JS frame regressions, and paired comparison correctness. The
  [performance audit](/quest/m2/audit-perf.md) repeats it before later cuts.
- [Perf](/quest/m2/perf/README.md) owns measured channel contention, relay
  allocations, ingest batching, worker quiescence and runtime selection.
- The [release audit](/quest/m1/release-audit/README.md) bounded the media
  reorder queues in [moq-dev/moq#6453](https://github.com/moq-dev/moq/pull/6453)
  and fixed QMux copying in
  [moq-dev/moq#6381](https://github.com/moq-dev/moq/pull/6381). MKV
  fragmentation below concerns parser replay before those queues.
- [Binding bytes](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-bytes.md),
  [foreign polling](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-poll-protocol.md),
  [Kotlin calls](https://github.com/moq-dev/rust2/blob/main/quest/m1/kt-ffi-calls.md),
  [NVENC pooling](/quest/m2/nvenc-pool.md), and
  [watch worker planning](/quest/m2/plan-watch-worker.md) retain their scopes.
- [Route wakes](/quest/m2/route-wakes.md) and
  [front parking](/quest/m2/origin-front-parks.md) own discovery scans/wakes.
- The [recurring audit](/quest/m2/audit-perf.md) retains real congestion-
  controller/loss-recovery coverage, broader importer/container coverage,
  and publish Fanout/audio-ring profiling. QUIC protocol microbenchmarks use
  `Unlimited`; their numbers cannot establish controller performance. Expand
  reassembly flight/reordering coverage there before proposing another index.
  [BBR study](/quest/m2/quic-bbr-study.md) owns controller behavior.
- The archive consumer replaced the timeline reader; benchmark it when
  revisiting archive reads.

## Required

- [Task capacity after bursts](/quest/m2/performance-survey/task-capacity.md) - polling and memory after peak task occupancy subsides
- [QUIC receive-state reuse](/quest/m2/performance-survey/quic-receive-reuse.md) - reassembly metadata retained by recycled completed streams
- [Cache metadata](/quest/m2/performance-survey/cache-metadata.md) - bookkeeping capacity after cached groups expire
- [Cache backing amplification](/quest/m2/performance-survey/cache-backing.md) - small retained samples pinning large shared fragments
- [Bandwidth grants](/quest/m2/performance-survey/bandwidth-grants.md) - repeated full allocation in Rust and JS
- [Cached JS group joins](/quest/m2/performance-survey/js-cached-groups.md) - repeated sorting while replaying retained groups
- [PCM batch processing](/quest/m2/performance-survey/audio-batches.md) - repeated tail movement during encoding and resampling
- [Fragmented MKV import](/quest/m2/performance-survey/mkv-fragments.md) - accumulated-buffer copies and parser replay
- [QUIC scheduling churn](/quest/m2/performance-survey/quic-schedule-churn.md) - priority and retirement work across unrelated groups
- [HLS request fan-out](/quest/m2/performance-survey/hls-serving.md) - repeated fetch, remux and playlist construction per viewer
- [Buffered browser groups](/quest/m2/performance-survey/hang-buffered.md) - per-frame scans and bulk skip work
- [Idle browser stats](/quest/m2/performance-survey/browser-idle-stats.md) - local transport polling without active publishing demand
- [Transport preference history](/quest/m2/performance-survey/transport-history.md) - full URL retention across retired sessions
- [Resume identity history](/quest/m2/performance-survey/resume-history.md) - historical source graphs held by first-seen track metadata
- [UDP socket queues](/quest/m2/performance-survey/socket-queues.md) - drop attribution and queue-size tradeoffs across workers and load
- [Session datagrams](/quest/m2/performance-survey/datagram-delivery.md) - complete per-object delivery across publishers and viewers
- [JWT verification](/quest/m2/performance-survey/jwt-verification.md) - key loading, lookup, parsing and crypto attribution
- [Announcement snapshots](/quest/m2/performance-survey/announced-snapshots.md) - HTTP polling cost versus result size and unrelated routes
- [Selective session operations](/quest/m2/performance-survey/session-selection.md) - exact-ID lookups and registry lock hold time
- [Browser media copies](/quest/m2/performance-survey/browser-media-copies.md) - CMAF sample and LOC framing copies versus retention
- [GStreamer ingest buffers](/quest/m2/performance-survey/gst-buffers.md) - copy avoidance versus upstream pool retention

## Related

- [Performance audit](/quest/m2/audit-perf.md) - nightly regression attribution and remaining coverage
- [Benchmark sources](/quest/m2/bench-sources.md) - publishing existing measurements to the trend table
- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - noise floor of the paired nightly regression report
