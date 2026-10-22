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

### Existing ownership and deliberately deferred work

- [Release performance](/quest/m1/release-perf/README.md) owns sparse gaps,
  resume duplicates, idle GC, stats drain, FETCH fan-out, JS frame regressions,
  and paired comparison correctness. Active PRs already address these.
- [Perf](/quest/m2/perf/README.md) owns measured channel contention, relay
  allocations, ingest batching, worker quiescence and runtime selection.
- [Release audit PR](https://github.com/moq-dev/moq/pull/6311) owns QMux
  copying/resource growth, CMAF allocation abuse, media reorder queues,
  watch teardown, and HLS contract investigations. MKV fragmentation below
  concerns parser replay before those queues; recheck that PR before coding.
- [Binding bytes](/quest/m2/ffi-bytes.md),
  [foreign polling](/quest/m2/ffi-poll-protocol.md),
  [Kotlin calls](/quest/m2/kt-ffi-calls.md),
  [NVENC pooling](/quest/m2/nvenc-pool.md), and
  [watch worker planning](/quest/m2/plan-watch-worker.md) retain their scopes.
- [Route wakes](/quest/m2/route-wakes.md),
  [keyed prefix polling](/quest/m2/keyed-prefix-polling.md), and
  [front parking](/quest/m2/origin-front-parks.md) own discovery scans/wakes.
- The [recurring audit](/quest/m1/audit-perf.md) retains real congestion-
  controller/loss-recovery coverage, broader importer/container coverage,
  and publish Fanout/audio-ring profiling. QUIC protocol microbenchmarks use
  `Unlimited`; their numbers cannot establish controller performance. Expand
  reassembly flight/reordering coverage there before proposing another index.
  [BBR study](/quest/m2/quic-bbr-natural-drain.md) owns controller behavior.
- Archive reader internals are being replaced by the
  [archive consumer](/quest/m1/archive/consumer.md). Do not optimize the
  outgoing timeline reader. Benchmark the landed replacement when revisiting it.

## Required

- [Fragmented QUIC sends](/quest/m2/performance-survey/quic-send-fragments.md) - packetization cost under a retained fragmented flight
- [Task capacity after bursts](/quest/m2/performance-survey/task-capacity.md) - polling and memory after peak task occupancy subsides
- [Bandwidth grants](/quest/m2/performance-survey/bandwidth-grants.md) - repeated full allocation in Rust and JS
- [Cached JS group joins](/quest/m2/performance-survey/js-cached-groups.md) - repeated sorting while replaying retained groups
- [PCM batch processing](/quest/m2/performance-survey/audio-batches.md) - repeated tail movement during encoding and resampling
- [Fragmented MKV import](/quest/m2/performance-survey/mkv-fragments.md) - accumulated-buffer copies and parser replay
- [QUIC scheduling churn](/quest/m2/performance-survey/quic-schedule-churn.md) - priority and retirement work across unrelated groups
- [HLS request fan-out](/quest/m2/performance-survey/hls-serving.md) - repeated fetch, remux and playlist construction per viewer
- [Buffered browser groups](/quest/m2/performance-survey/hang-buffered.md) - per-frame scans and bulk skip work
- [Session datagrams](/quest/m2/performance-survey/datagram-delivery.md) - complete per-object delivery across publishers and viewers
- [JWT verification](/quest/m2/performance-survey/jwt-verification.md) - key loading, lookup, parsing and crypto attribution
- [Announcement snapshots](/quest/m2/performance-survey/announced-snapshots.md) - HTTP polling cost versus result size and unrelated routes
- [Selective session operations](/quest/m2/performance-survey/session-selection.md) - exact-ID lookups and registry lock hold time
- [Browser media copies](/quest/m2/performance-survey/browser-media-copies.md) - CMAF sample and LOC framing copies versus retention
- [GStreamer ingest buffers](/quest/m2/performance-survey/gst-buffers.md) - copy avoidance versus upstream pool retention

## Related

- [Performance audit](/quest/m1/audit-perf.md) - nightly regression attribution and remaining coverage
- [Benchmark sources](/quest/m2/bench-sources.md) - publishing existing measurements to the trend table
- [Nightly comparisons](/quest/m1/bench-ci.md) - paired nightly regression reporting
