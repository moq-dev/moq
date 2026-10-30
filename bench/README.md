# Benchmarks

This directory contains repository-level benchmark orchestration. Rust
microbenchmarks stay beside their crates under `rs/*/benches`, while the
`moq-bench` load generator and host sampler stay in `rs/moq-bench`.

`run.sh` owns builds, comparison rounds, and reporting. `relay.sh` owns the
temporary relay lifecycle and load execution shared by each comparison mode.
A temporary relay is ready only once its PID owns both the TCP listener and UDP
socket on its assigned port. `lsof`, included in the Nix shell, verifies
ownership. A collision uses the existing startup attempts; exhausted attempts
fail with the relay log before any load starts.

## Commands

Run every Criterion target plus the local relay workloads:

```bash
nix develop --command just bench
```

Measure small-group session cost over independent publisher and viewer counts,
for SUBSCRIBE and FETCH:

```bash
nix develop --command cargo bench --locked -p moq-net --features fuzz --bench session -- session_delivery_group_cost
nix develop --command env SESSION_CPU=1 cargo bench --locked -p moq-net --features fuzz --bench session -- session_delivery_group_cost_cpu
```

Run the same session scenarios through relay admission, auth, cluster routing,
and stats over the in-memory transport:

```bash
nix develop --command cargo bench --locked -p moq-relay --features test-support --bench session -- relay_session_dash_
nix develop --command cargo bench --locked -p moq-relay --features test-support --bench session -- relay_session_delivery_group_cost
```

The relay and model targets share shapes and complete-payload assertions. The
`relay_` prefix distinguishes results; the model-only idle case needs its server
session handle and is not registered by the relay target. Relay fixtures enable
stats and admit all benchmark paths using embedded auth. The dash viewer gets a
peer grant to discover hidden stats routes on protocols without hidden opt-in. The announcement limit
uses the existing peer limit so the large broadcast sweeps exercise routing
rather than ordinary-client admission caps. These settings are benchmark-only.
The relay target has its own Nightly leg.

The CPU mode requires Unix and measures the benchmark's current thread, including
its protocol drivers and the flow-control fixture but excluding scheduler
preemption. Both modes exclude setup and cache-clock advancement. Constrained
cases hold data after the first transport write and release it only after a
write actually blocks; Lite FETCH uses a bidi response, while IETF FETCH and
SUBSCRIBE use uni data streams. No sleep supplies backpressure. Every case
checks complete payload delivery. Compare paired runs with identical settings
and disclose host load; CPU timing still depends on hardware contention.

Measure 50 fps audio at per-packet, 100 and 200 ms grouping, plus 400 fps
(2.5 ms packets) at per-packet and the 20 ms default. Frames are 200 bytes:

```bash
nix develop --command just bench-audio
```

This sweeps room connections (16, 32) and subscriptions per connection (2, 8)
independently, plus one publisher serving 64 and 200 subscribers. It uses the
normal relay sampler and runs nightly. Each shape reports delivered frames,
p99 latency, relay CPU and RSS; the group-size suffix is the number of frames
after the keyframe (0, 4, 9), so each group carries 1, 5 or 10 frames. These
synthetic payloads measure group overhead; they do not measure codec loss
concealment.

Compare the current tree with another revision:

```bash
nix develop --command just bench origin/main
```

Compare one multi-threaded Tokio runtime with the same number of independent
Tokio/epoll and io\_uring workers:

```bash
nix develop --command just bench-runtime
nix develop --command just bench-runtime 5 16
```

Runtime comparison requires Linux because io\_uring and relay process metrics
come from Linux interfaces. The default worker count is the number of logical
CPUs the run may use.

Measure `moq-uring`'s actual `Local` thread guard separately from I/O:

```bash
nix develop --command cargo bench --locked -p moq-uring --bench worker_turns -- local_thread_check
```

This compares the same constant-returning transport method directly and through
`Local`. It isolates the thread guard; it does not attribute runtime workload CPU
to that check or disable the production safety guard.

Sweep the relay's QUIC shard count against the connection count, for the
room (`video`: every connection publishes and subscribes) and `fanout` (one
publisher, the rest subscribers) workloads:

```bash
nix develop --command just bench-shard
nix develop --command just bench-shard "0 1 8 24" "64 512"
```

Zero shards is the shared runtime with one Tokio thread per usable CPU; any
other count is that many unpinned Tokio workers. The table reports CPU per
connection, so a steering, dispatch, or per-shard cost that grows with either
axis reads as a slope. Linux only, since nothing else steers a reuseport
group. It runs nightly.

Profile the relay under a workload, in one or more capture modes: `lock`
(worker std mutex waits by stack), `cpu` (CPU stacks of every relay thread),
or `heap` (jemalloc retained heap and allocation churn):

```bash
nix develop --command just bench-profile lock
nix develop --command just bench-profile "cpu heap" fanout-heavy "4 16"
nix develop --command just bench-profile "cpu heap" audio "4 16" io-uring-workers
nix develop --command just bench-profile cpu chat 4 io-uring-workers origin/main
```

This builds the relay with `[profile.profiling]` and frame pointers, then runs
the workload on the selected runtime (`tokio-workers` by default, or
`io-uring-workers`) at each count twice per mode: an unprofiled control, then a
profiled run, so the throughput and CPU difference shows the profiler's overhead. Each mode captures only the relay process, never the load
generator, during the profiled run's 5 s steady window, the same window as the
summary's throughput. Linux only. Both runtimes use the runtime comparison's
feature build; io\_uring uses its kernel support check too. Readiness verifies the
selected backend and exact worker count from the relay's actual thread names,
retained as `runtime.json`, before the generator starts.

The optional final argument selects a base revision. The current harness builds
it in a temporary worktree and drives both revisions with the current generator,
sampler, workload and capture settings. This also works when the base's own
profiling harness has no runtime selector. Results live under `relay/base/` and
`relay/current/`, with one unprofiled control per revision, worker count and mode.

### Lock wait

During the profiled run's steady window, `perf record` samples every
worker switch-out (the `context-switches` event with a period of 1) with a
DWARF-unwound stack, plus each switch-in, so every off-CPU interval has an
exact duration. glibc's `syscall` wrapper keeps no frame pointer, so
frame-pointer unwinding alone would drop `Mutex::lock_contended`; without
frame pointers in the relay, perf's DWARF unwinder stops at the first relay
frame. Both are needed. An interval counts as a mutex wait only when the worker is blocked in
the futex under std's `Mutex::lock_contended` (`kio::Lock`). Condvar,
allocator and parking waits are counted separately, and spinning is excluded.
CPU runs from each switch-in to the sampled switch-out, where the next wait
begins, clipped to the same recorded window as waits.
`/proc` schedstat only identifies workers that slept throughout: a silent worker
whose counter advanced is ambiguous and refused. A worker with switch records
but no sampled switch-out is refused too. `lock.json` version 3 uses this
recorded CPU denominator; wait/CPU can exceed one.
`lock/mutex.folded` holds the blocking stacks, weighted by microseconds
waited, in flamegraph folded format.

On an arm64 host with SVE, perf's DWARF unwinding requests the SVE `VG`
register, which the kernel refuses for a software event, so the lock recording
fails to open; profile lock waits on x86.

### CPU

`perf record` samples every relay thread with the `cpu-clock` event at 999 Hz,
unwinding by frame pointer. Kernel time stays in the profile as a `[kernel]`
leaf under the user stack that entered it, so `sendmsg` and `recvmsg` cost
lands on its caller; kernel frames themselves are discarded. A frameless libc
leaf (`memcpy`, the syscall wrappers) hides only its direct caller.
`cpu/cpu.folded` holds the stacks in flamegraph folded format, each rooted at
the thread's role (`moq-quic-0..N` fold into `moq-quic`). `cpu/cpu.json`
counts samples per thread; its `sampled_cores` should track the host sampler's
relay CPU, which shows the capture saw the whole process. A capture without
resolved MoQ frames is refused.

Both perf modes sample in kernel context, and per-thread ring buffers exceed
the unprivileged locked-memory limit, so the recipe refuses before building
until `perf_event_paranoid` is -1. A recording that still drops events is
refused rather than reported.

```bash
sudo sysctl -w kernel.perf_event_paranoid=-1
```

The maintainer's 24-core `desktop` workstation is the profiling host: it
keeps `kernel.perf_event_paranoid=-1`. Profile there only while it is quiet,
with no other agents running and load near zero, since other work on the
host shows up as lock waits and CPU the relay did not cause.

### Heap

The heap mode builds the relay with `moq-relay/jemalloc` (the other modes keep
the default allocator) and starts it with jemalloc profiling active from
startup, accumulating allocations, at one sample per 64 KiB allocated. Once
the relay has installed its SIGUSR1 handler, `rs/moq-tokio/src/jemalloc.rs`
dumps a snapshot on each signal: `before` the load connects, `steady-start` and
`steady-end` around the steady window, and `teardown` once the relay's internal
`/sessions` list is empty after the generator closed its sessions (refused if
that takes over 15 s). `jeprof`, from the dev shell, symbolizes them:

- `heap/heap.json`: retained bytes at each snapshot, and allocation churn
  (bytes and allocations per second) between the two steady snapshots.
- `heap/churn.folded`: what the steady window allocated, by stack.
- `heap/retained.folded`: what the heap held at `steady-end`.
- `heap/teardown.folded`: what the load left behind after teardown, net of
  `before`; a leak candidate when it grows.

All are jemalloc's sampled estimates. A run without resolved MoQ frames in its
churn is refused.

## Workloads

The `workloads/` TOML files contain only traffic shape. The harness supplies the
temporary relay URL, TLS settings, startup ramp, run duration, reporting
interval, and output paths so every runtime receives the same load.

- `video`: light many-to-many video traffic.
- `fanout`: light one-to-many traffic.
- `video-heavy`: multicore many-to-many video traffic.
- `fanout-heavy`: multicore one-to-many traffic near saturation.
- `chat`: one broadcast to 2,000 viewers, one small message per group.
- `audio` and `audio-fanout`: small, 50 fps audio-shaped frames; `bench-audio`
  sweeps grouping and load shape through command-line overrides.

The runtime matrix rotates execution order between rounds, then reports the
median throughput, loss, latency, CPU split, context switches, RSS, and thread
count. Compare CPU only when delivered throughput and loss are equivalent. A
runtime that falls behind can use less CPU simply because it completed less
work.

Benchmark output is informational and machine-specific. Crashes, zero delivery,
and invalid samples still fail the command.

On a hybrid CPU the same case measures far apart on performance and efficiency
cores, so `run.sh` pins itself, and every build and benchmark process it starts,
to the performance cores wherever the kernel lists them
(`/sys/devices/cpu_core/cpus` on Intel hybrid parts), within the CPUs it was
already allowed, so an outer `taskset` or cpuset still narrows it. The run prints the pinned cores beside its result and records
them as `affinity` in the metadata. A CPU with one core type runs unpinned.

BASE comparisons run paired rounds (default three, `BENCH_RUNTIME_ROUNDS`),
alternating base/current order. Startup remains outside the final five-second
throughput window. `BENCH_ARTIFACTS` chooses the evidence parent (default
the current worktree's `.tmp/bench`), which must be outside the repository,
or git-ignored and outside `bench/`; each invocation retains a new run directory even on failure.
It contains raw load/host JSONL, process diagnostics, Criterion statistics,
execution order, source revision and tracked dirty patch, tool/build/machine
metadata, workload files, complete stdout/stderr, and a separate copy of untracked
source files. The retained path is printed at the end, including on failure.
Choose an evidence parent outside disposable worktrees when the report must survive
worktree removal. Criterion case discovery uses isolated short measurements and
their full JSON IDs, since `--list` truncates display names; discovery samples
are retained separately and never used in the comparison. A target that writes
no Criterion samples, such as one that prints its own table, is listed as skipped.

Each comparison reports the median of the paired percentage deltas, their median
absolute deviation and range, and flags fewer than three observations or a sign
that varies across pairs. MAD is descriptive dispersion, not a confidence
interval; three rounds cannot establish a small improvement. Timing remains
informational. Loss and latency are explicitly lifetime statistics; throughput
and CPU use the same load-window boundaries. On Linux, one host sampler observes
both the relay and the generator, then stops the generator after the measurement
ends. CPU counters are linearly interpolated between the surrounding 500 ms
samples; this is a sampled estimate, not an exact accounting boundary. Raw
per-thread samples and CPU rates for threads present at both boundaries are
retained; short-lived threads remain represented in process CPU totals.

Generator CPU is reported beside relay CPU because sharing a loopback host can
hide a relay improvement behind generator saturation. CPU efficiency remains
withheld until offered load and delivery/loss are comparable. Zero-baseline
metrics retain absolute deltas when their percentage ratio is undefined.

Each wrapper carries a runner of the binding benchmark, which measures what the
binding adds over Rust: `just go bench`, `just py bench`, `just kt bench`,
`just cpp bench`, `just dart bench`, and `just swift bench` (macOS), beside the
Rust baselines in `rs/moq-ffi/src/bench.rs`. Every runner runs the same
in-process rounds with no network: each publisher writes one group from the
driving thread while every subscriber reads it on its own task, and a round
waits for every subscriber. It sweeps frame sizes from 100 B to 1 MiB at one
publisher and subscriber, then 1/4 publishers by 1/4 subscribers at 1 KiB, and
times two sync calls (`sequence`, `info`). Each case reports delivered MB/s and
process CPU (user + system, every thread) per MB into
`$MOQ_BENCH_DIR/<runner>.json`. `MOQ_BENCH_SECONDS` sets each case's time
budget (default 1); every received frame is checked byte for byte.

The binding benchmark itself lives in
[rust2](https://github.com/kixelated/rust2/blob/main/bench/ffi/README.md), with
the UniFFI fork whose changes it measures: its `just bench-ffi` runs these
runners nightly at a pinned moq revision, against the fork, and reports each
wrapper's ratio to Rust. So that they keep working here, moq's Nightly runs every
runner at a token budget without measuring it: `just gh nightly bindings` on
Linux, and `just swift bench` in the macOS Platform job.

## Browser

Measure the JS transport, CMAF, and publish/watch paths in a real browser over
real WebTransport, against a temporary relay:

```bash
nix develop --command just bench-browser
nix develop --command just bench-browser origin/main
nix develop --command just bench-browser --smoke
```

`bench/browser/run.sh` bundles one set of pages per variant: `current`, `aa` (an
identical second bundle, so the A/A spread shows the noise floor), `copy` (one
extra copy of every received stream chunk injected into `@moq/net`, the control
that shows the suite can see a copy-heavy change), and `base` when a revision is
given. Each variant resolves `@moq/*` from its own checkout. Variants alternate
within each round, with the starting variant rotated, after one warmup round
(`--rounds`, `--warmup`, `--scenarios transport,cmaf,media`).

- `transport`: a publisher page writes 160 B through 1 MiB frames, a consumer page
  subscribes and reads them all. Connection setup and fixture generation stay
  outside the timed interval, which starts at SUBSCRIBE.
- `cmaf`: CMAF fragment decode of fixed audio (50 x 160 B) and video
  (a 64 KiB keyframe and 29 x 4 KiB) fixtures. Fixture loading stays outside
  the timed interval; no encoding or network runs in that interval.
- `media`: `@moq/publish` encodes a scrolling 1280x720 canvas at 30 fps (H.264,
  2.5 Mb/s, 1 s keyframes) and `@moq/watch` plays it on a 2D canvas. After a
  warmup, a 10 s window reports published, received, decoded and presented frame
  rates, decode backlog, frames decoded but never painted, decoder queue depth,
  long tasks, and the watcher's heap slope. Frame counters are read at the
  window's edges, so decoded-but-unpainted can be off by a frame or two; decoder
  queue depth (mean, and maximum within the window) comes from a thin
  `VideoDecoder` wrapper present in every pass.

Costs are split by where they run. Main-thread CPU is CDP `TaskDuration` in
thread ticks; process CPU is each page's renderer (every thread, including
software decode) plus the browser's shared network service process, where
WebTransport and QUIC run, and the GPU and browser processes. Separate passes
keep instrumentation out of timed samples: a traced pass retains a gzipped Chrome
trace per workload and reports GC pauses and reclaimed JS heap; a counted pass
wraps typed-array constructors, `set` and `slice` to report JS-visible bytes
copied and allocated per payload byte during delivery, with connection setup
subtracted; a probed pass reads each presented
frame's painted id back to report frame delay percentiles. Native copies inside
WebTransport and WebCodecs are unavailable, and the report says so.

Chromium runs headless with `--disable-gpu`, so decode and rendering are
software on every host, and with background throttling disabled. A page that is
not visible fails the run. `report.json` keeps every raw sample with host load,
the browser version, flags and per-page environment, source revisions, a
per-variant summary, and paired per-round deltas against `current` with their
range and whether the sign held. The pairs are `current` against `base`, and each
control (`aa`, `copy`) against `current`. `BROWSER_BIN` runs an identified local
browser instead of Playwright's Chromium.

Read process CPU with its granularity in mind: Chromium reports it in 10 ms ticks,
so a sub-second transport sample carries a few percent of quantization; main-thread
CPU has microsecond resolution. The renderer's process CPU minus its main thread
is mostly software decode, compositing, and GC helper threads. On a shared host the
A/A spread is wide, so a single extra copy shows reliably in the counted pass, not
in timing; compare timing only across a change larger than the `aa` range.

Every transport frame carries its index and a patterned payload: timed passes
check sampled bytes, the counted pass checks every byte, and frame counts and an
order-independent digest must match, so skipped work cannot read as a speedup.
CMAF output is decoded and checked byte for byte in a separate call after each
phase's CPU and trace snapshots.
Crashes, wrong bytes, missing frames, a window that presents nothing, or an
unavailable CPU clock fail the run; timing never does. The nightly runs
`--smoke` and keeps its run directory as the `bench-browser` artifact.

The in-memory frame, retained-history, and gap ledger paths also run directly in Chromium:

```bash
nix develop --command bun bench/browser/micro.ts --bench frames --out .tmp/frames.json base=/path/to/release current=. aa=.
nix develop --command bun bench/browser/micro.ts --bench drift --out .tmp/drift.json before=/path/to/main current=. aa=.
nix develop --command bun bench/browser/micro.ts --bench ledger --out .tmp/ledger.json before=/path/to/main current=. aa=.
```

Each variant uses its own workspace source and a persistent page, with one full
warmup before three alternating measured rounds. `aa` must bundle identically to
`current`. The JSON preserves every CSV sample and the browser version. `frames`
runs the checkout's existing `js/net/bench/frames.ts`; `drift` uses the same fixture
against every checkout, sweeping retained groups, publishers, and readers. Replay
and group setup stay outside drift timing; each parked read checks its live
payload. `ledger` uses one fixture against every checkout's internal ledger, sweeping
ascending, descending, shuffled, and rolling-expiration arrivals over the retained
gap count. It includes each arrival's expiration and coverage checks.
The drift sweep includes untimed controls and timed groups with decode-order
B-frames, independently varying publisher and reader counts and retained history.
Its `time_mode` column is 0 for untimed, 1 for millisecond timestamps, and 2
for mixed millisecond/microsecond timestamps, which exercise exact cross-scale
comparison.

The browser smoke lane preserves one measured round of all three fixtures, and drift
and ledger also run in the nightly JS benchmark leg. Timing is
observational, with the A/A spread showing the host's noise floor.

## Pattern and container coverage

Rust Criterion coverage includes `moq-pattern`'s
`matching` (pattern count and path depth, last match and miss) and `moq-mux`'s
`containers` (CMAF and Annex-B import/export). Container cases vary frame count
and payload size; CMAF also varies track count. Separate frame and byte rates
use the same fixtures, with byte rates counting the encoded stream size.
Fixture generation, publisher setup and destruction stay outside timing.
The shared Rust benchmark nightly invocation measures both targets in quick mode.

## Nightly trends

<https://git.moq.dev/bench/> is one static table of every nightly series,
linked from the [nightly reports](https://git.moq.dev/) root with every other
report. Each row shows the newest value, its baseline, the change, and a
sparkline of the last 30 nightly points. The baseline is the median of the
previous 7 points. A change beyond both 10% of the baseline and 3 times those
points' median absolute deviation is flagged, so each series' own noise sets its
bar: red is worse and green is better, by the series' direction (time, CPU,
size, and ratios lower; throughput higher). Flagged rows open at the top, worst
first; everything else sits in a collapsed section per source, behind a text
filter. A series with fewer than 8 points is grey.

A failed or cancelled source records no point, so its section is grey and
marked stale with the date and commit of its last measurement; it is never
flagged as tonight's result. A series missing from its source's newest run was
retired and leaves the table. Re-runs of the same commit collapse to the latest
point.

The Nightly workflow keeps history on one `gh-pages` branch, where
`github-action-benchmark` appends each night's points to `bench/{costs,rates}/data.js`
with its alerts, comments, and charts unused. The publisher runs only on `main`,
serializes history writes, renders the table from that history even when every
source failed, and deploys the branch explicitly. Re-running its job in place
fails, because the earlier attempt's `github-pages` artifact remains; delete
that artifact first or wait for the next Nightly.

The table includes these measurements:

- Rust Criterion mean ns per iteration; the history also keeps the confidence interval.
  The `net-cpu` source and the moq-quic targets time thread CPU; the others
  time elapsed time. The Rust bench target jobs measure them with a short warmup
  and Criterion's quick mode. Per-group benchmark settings can override the default warmup. Quick estimates
  trade precision for nightly runtime; the paired comparison below assesses a change.
- JS benchmark CSV time columns (`ns`, `us`, `ms`, and their per-operation
  variants) and `size_bytes` columns, retaining every other column as a workload
  dimension in each series name.

Hosted runners vary across nights, so a trend flag prompts a look at the paired
comparison rather than proving a regression.
Malformed, empty, nonfinite, or duplicate measurements fail validation. The
nightly artifacts retain the reports, raw JS output, and each Criterion run's
metadata and estimates for 45 days. Relay load, audio quality, and the Rust
benchmark tests job's nextest output do not enter the table.

Render the table from the published history:

```bash
git fetch origin gh-pages
git worktree add .tmp/pages origin/gh-pages
nix develop --command just gh benchmark-report index .tmp/pages
```

Then open `.tmp/pages/bench/index.html`. Without `--commit`, the newest
recorded commit counts as tonight's. Reproduce the measurements locally:

```bash
nix develop --command just gh nightly rust-bench
nix develop --command just gh nightly js
```

These write `target/bench/nightly/{rust-bench,js}/report.json`. For a focused
measured Rust run using the same evidence lifecycle:

```bash
nix develop --command env BENCH_LABEL=pattern BENCH_RUNS=/tmp/criterion-runs.txt just gh criterion -p moq-pattern --bench matching
nix develop --command just gh benchmark-report rust /tmp /tmp/rust-report.json
```

A new source uploads a `bench-*` artifact containing `report.json`, a nonempty
JSON array of `{ "name": "case/dimensions", "unit": "ns", "value": 123 }`,
and `outcome.json` containing `{ "status": "success" }`. Names must be unique
within a source; dimensions belong in the name. Supported units are `ns`, `us`,
`µs`, `ms`, `s`, `ms/MB`, `x`, `bytes`, `B`, `MiB`, `MB/s`, `MiB/s`, `B/s`,
`ops/s`, and `frames/s`. Optional `range` and `extra` fields retain uncertainty
and context. Validate and split sources into cost and throughput histories with
`just gh benchmark-report prepare INPUT_DIRECTORY OUTPUT_DIRECTORY`; extending
the table requires no new renderer or data branch. A source's `paired.json`, as
`just gh benchmark-report rust` writes for a paired run, joins the paired comparison.

### Paired comparison

Each Nightly's Rust bench groups (`rust-bench`, `rust-shared`, `rust-extra` and
`rust-uring`) also build the previous scheduled Nightly's commit beside their
own, on the same runner, so night-to-night runner variance drops out. Each
built target runs once per side in the trend's quick mode, alternating which
side goes first; the current side is the trend point. A case more than 10%
slower in that screen reruns, worst first and at most 20 per group, in three
alternating rounds with the BASE comparison's settings (1 s warmup, 2 s
measurement, 30 samples). A case whose rounds are all slower with a median
beyond 10% is a confirmed regression and fails its job after the upload, so the
Alert workflow posts the failed Nightly. A target the base cannot build or run
fails the job too, rather than passing uncompared; a bench target the base does
not have yet is only reported as new. The threshold is a starting point until
A/A runs on the hosted runner measure its noise floor.

The table page shows tonight's comparison above the trend: confirmed
regressions, targets not compared, cases faster in the screen, slowdowns the
rounds rejected or the budget skipped, and the rest. `bench/paired.json` on the
site holds every case. A dispatched Nightly's `bench-base` input compares
against another revision; its own commit gives an A/A run.

Reproduce a paired group locally against any revision; `previous-nightly`
resolves the last scheduled Nightly's commit through `gh`:

```bash
nix develop --command env BENCH_BASE=previous-nightly just gh nightly rust-bench
nix develop --command just gh benchmark-report gate target/bench/nightly/rust-bench
```

Each run's evidence directory keeps both builds' estimates (`criterion` and
`criterion-base`), the confirmation rounds under `confirm/`, and the run's
`paired.json`.

### CodSpeed

The CodSpeed workflow adds instruction counts for every `main` commit that
touches Rust, at <https://codspeed.io/moq-dev/moq>. Its CPU simulation runs
each bench under Valgrind on a hosted runner, so a change of a few percent
shows without the paired comparison's rounds. It does not see syscalls, I/O,
contention, or caches, and skips `iter_custom` benches, so `just rs codspeed`
builds only single-threaded targets without I/O, and Nightly keeps measuring
every bench. Workspace benches import `criterion`, which `rs/bench-harness`
resolves to Criterion, or to CodSpeed's instrumented Criterion under
`cargo codspeed`. Nothing gates on it.

```bash
nix develop --command just rs codspeed
nix develop --command cargo codspeed run
```

Run locally, the benches only check that each case runs; there is no
measurement outside CodSpeed's runner.

## Local check capacity

Prepare the same warm dependencies and artifacts in each isolated checkout, then
compare solo and concurrent suites with their actual worker counts:

```bash
just bench-local --checkout "$PWD" --checkout /path/to/another/checkout \
  --workers 1 4 8 24 --samples 3 --cache warm -- \
  cargo nextest run --locked -p moq-net --lib
```

Each JSON record retains host load, configured workers and lanes, admission
delay, work wall time, total wall time, and child CPU time. Logs stay under each
checkout's `.tmp/local-bench/`. To compare Bun workspace fanout, repeat with
the same `bun run --filter=... test` command, then add `--sequential`. To compare
the module lanes `just check` runs at once, sweep `--jobs 1 3` over
`bash sh/dispatch.sh check BASE`, which is then admitted once as a whole.
Admission remains outside the suite's timeout clocks. Use separate checkouts to
avoid measuring Cargo's artifact lock instead of competing work.
