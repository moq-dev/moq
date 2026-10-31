# Main is no slower than release

## Goal

Before the next release cut, every benchmark where `main` is slower than
`release` is either fixed or accepted with a recorded reason. Accidental cost
is fixed. Cost that pays for a feature `main` deliberately added is accepted,
and the reason is written here.

This epic's own work: once its child has landed, re-pair the relay `fanout`
workload against `release` and confirm main's relay CPU is no higher in every
round. The sparse rows are accepted (2026-10-30). The browser frame path
needs no repeat; the [performance audit](/quest/m2/audit-perf.md) now pairs
every later cut with `release`.

## Plan

Decided 2026-10-21, when planning from a paired comparison of `release`
(`0c56e5b4e`, which included the bench backport
[#6103](https://github.com/moq-dev/moq/pull/6103)) against `main`
(`8b0d9952f`). The run used 811 paired Criterion cases, three alternating
confirmation rounds for the largest moves, JS on bun, and the relay load
harness. It ran on a shared 8-core host, so a small wall-time change may be
noise.

- The epic gates the cut through the [m1 milestone](/quest/m1/README.md),
  because `main` must not ship slower than `release` without a decision.
- In scope: the five cases that held through confirmation, plus the large
  screen-only moves. The screen-only moves are re-measured with callgrind
  instruction counts, and any that hold are fixed.
- Only accidental cost is fixed. Cost from an intended feature is accepted,
  and the reason is recorded here.
- The IETF `/fetch` rows of `session_delivery_group_cost` are accepted and
  dropped from release pairing. They measure different work on each branch:
  release's bare track handle opened a live SUBSCRIBE, while main sends a real
  FETCH per viewer ([#5820](https://github.com/moq-dev/moq/pull/5820) made
  handles lazy, and [#4974](https://github.com/moq-dev/moq/pull/4974) added
  fetch-only demand). The lite `/fetch` cost was real: a relay sent one
  upstream FETCH per group, but revived the parked logical track for every
  viewer's FETCH. [#6286](https://github.com/moq-dev/moq/pull/6286) serves a
  whole cached group from the parked copy instead, cutting 12 to 19% of the
  instructions per viewer where viewers share a group. The rest of the gap to
  a viewer that holds a query is accepted: each viewer's own fetch of a group
  it lacks revives its logical track, so the open group keeps its route and
  fails over mid-read, and its copy releases the route's demand once idle.
- `stats/session-report` is accepted at O(live sessions), after a cheap fix.
  Per-label payload bytes ([#5731](https://github.com/moq-dev/moq/pull/5731))
  match payload billing per label, such as per signing key. After removing
  redundant row locking and live-session CAS, three alternating measurements
  give 52.2 us for 10,000 sessions without wires and 388.4 us with one real
  transport wire each. A once-per-second drain uses about 0.04% of a core
  with those wires; the earlier under-0.01% estimate omitted their walk.
  This remaining per-session and per-connection sampling is accepted. Deleting
  per-label bytes was rejected.
- `track_gc/idle` is accepted at about 20 ns, flat across track counts, after
  [#6291](https://github.com/moq-dev/moq/pull/6291) skipped the clock read
  with no wall deadline and reused the bench's waiter (36.7 ns before). The
  remaining ~8 ns over release's 11.6 ns is the lock and driver registration
  that the wall-clock aging from
  [#5548](https://github.com/moq-dev/moq/pull/5548) needs. An earliest-deadline
  atomic cannot prove a replacement driver is registered, and a lock-free
  notification primitive is not worth ~8 ns.
- `track_gap_flush` is accepted at O(log gaps + retired entries) per shared
  successor, after [#6287](https://github.com/moq-dev/moq/pull/6287) indexed
  the gap ledger and retired a shared verdict's gaps with one split. Each
  retired entry must be freed exactly once, so the batch is already amortized
  optimal; a representation that hides that reclamation would widen scope for
  no measured benefit. Sparse arrivals cost O(log gaps) amortized, and at 4096
  gaps flush, read, and expiry instructions fell 80 to 95%.
- The mux exports' and Annex B import's fixed cost per broadcast is accepted.
  It publishes required output: a per-track timeline and an immediately
  completed catalog record. Their cold DEFLATE state is most of it (81% of a
  one-frame import's instructions are its zero-fill), and the encoders already
  build that state lazily on first use. It is paid once per broadcast: the
  import ratio falls from 3.97x at one frame to 1.49x at 64. TS export's
  growth with track count is its T-STD schedule (see the final run).
- The model's remaining per-group and per-frame cost is accepted, after
  [moq-dev/moq#6294](https://github.com/moq-dev/moq/pull/6294) cut the
  accidental part (a second track upgrade per first frame, a 128-bit division
  per buffered datagram on join, and draining empty waiter lists on every
  write). What remains pays for features: the arrival stamp each frame reads
  from the clock ([#6249](https://github.com/moq-dev/moq/pull/6249)), largest
  notifications ([#5725](https://github.com/moq-dev/moq/pull/5725)), the
  rewind check and starts index each first frame fills
  ([#5803](https://github.com/moq-dev/moq/pull/5803),
  [#6074](https://github.com/moq-dev/moq/pull/6074)), indexed expiry wakes,
  the bounded wall-clock expiry scan per append, and a subscriber's ranges and
  datagram max delay ([#5195](https://github.com/moq-dev/moq/pull/5195)).
  None of it grows with the sweeps. `track_stamp_groups/sparse` stays highest
  at 4.4x, because release's first frame skipped nearly all of that work.
- Session delivery is accepted at 1.28x to 1.77x in instructions. It is the
  per-group and per-frame cost above on every hop, measured through the
  flow-controlled mock transport
  ([#5530](https://github.com/moq-dev/moq/pull/5530)) that release's harness
  lacked.

### Release as a baseline

`release` lacks many of the features the backported benches exercise:
per-reader delivery records, wall-clock aging, the gap ledger, rewind checks,
and epochs. So a ratio against `release` shows where to look, not what to fix.
Main's own sweeps over a size parameter are the better regression signal: a
cost that grows with the sweep is accidental, while a flat per-operation cost
usually pays for a feature.

### Confirmed (three rounds, ratio is main / release)

| Bench | release | main | Ratio | Owner |
|---|---|---|---|---|
| `track_gap_flush/4096` | 5.7 us | 551 us | 96x | accepted (above) |
| `track_gap_flush/512` | 1.8 us | 48 us | 27x | accepted (above) |
| `track_gap_flush/64` | 1.5 us | 6.2 us | 4.1x | accepted (above) |
| `stats/session-report/1000x10` | 11.0 us | 86.8 us | 7.9x | accepted (above) |
| `track_gc/idle/16384` | 11.6 ns | 73.7 ns | 6.3x | accepted (above) |
| `origin/resume_overlap/sequence/1r_64g_32c` | 93 us | 426 us | 4.6x | fixed in [moq-dev/moq#6292](https://github.com/moq-dev/moq/pull/6292) |
| `session_delivery_group_cost/moq-transport-22/.../viewers=64/.../fetch` | 1.19 ms | 3.86 ms | 3.2x | accepted (above) |
| `group_write_frames/single/8192` | 424 us | 486 us | 1.17x | accepted (above) |

Main is much faster on subscriber churn, joins, range demand, and idle
sessions (by 100x to 3000x). The relay load test used 21% less CPU on video
and 12% less on fanout, with the same delivery.

### Screen only, re-measured

Re-measured on 2026-10-22 with callgrind instructions per operation, release
rebuilt under main's dev shell with fat LTO. Ratios are main over release;
the moq-net and kio rows include the fixes above.

| Bench | Screen | Instructions | Owner |
|---|---|---|---|
| `container-export/ts/4x1`, `mkv/4x1` | 4.7x, 3.9x | 1.70x, 1.59x | accepted (above) |
| `annexb-import`, 1 and 64 frames | median 2.1x | 3.97x, 1.49x | accepted (above) |
| `track_stamp_groups/append/64`, `sparse/64` | median 2.7x | 2.21x, 4.43x | accepted (above) |
| `track_parallel_write/1` | 1.6x | 1.99x | accepted (above) |
| `track_fanout_frame/1` | 1.42x | 1.86x | accepted (above) |
| `track_empty_run/{fresh/1,stale/8}/512` | median 1.36x | 1.53x | accepted (above) |
| `track_datagram_subscriber_join/0d/1`, `64d/64` | median 2.3x | 2.12x, 3.55x | accepted (above) |
| `track_sparse_read/timed/4096` | 4.3x | | fixed in [moq-dev/moq#6287](https://github.com/moq-dev/moq/pull/6287), regressed again (final run) |
| `origin/announce_duplicate`, `1d_1s` to `240d_240s` | 4.3x | 1.06x to 1.16x | noise |
| `session_delivery_{relays,paced,scale}` | 1.43x to 1.56x | 1.28x to 1.77x | accepted (above) |
| moq-stats `aggregate`, live=16, departed=4096 | 2.1x | 1.05x | noise |
| kio `contended_{notify,log}`, 16 threads | 4x to 5x | wall 1.04x to 1.15x | noise |
| JS `net/bench/frames.ts` on bun | 1.2x to 2.2x | | accepted (below) |

The kio contention benches spin across threads, so instructions don't measure
them; the row is the least loaded of three alternating wall-time rounds, and
later rounds swung both ways.

JS `net/bench/frames.ts` on bun had lite rows at 1.2 to 1.3x and IETF rows at
1.3 to 2.2x. Fixed in Chromium:
[#6296](https://github.com/moq-dev/moq/pull/6296) removed a retained-history
scan, and [#6479](https://github.com/moq-dev/moq/pull/6479) stopped
releasing a finished stream's reader lock, which built a `TypeError` per
group stream. Every row then sits within the A/A spread of release.
The group write/read microbench stays 3 to 12% above release against an
A/A of 3 to 8% on the same host. Decided 2026-10-23 (maintainer): browser
parity accepted on this evidence; the 2026-10-25 run confirmed it.

### Final run (2026-10-25)

`release` (`3db33f6715`) against `main` (`018f1c46e5`): 1,019 paired
Criterion cases, three relay load pairings, and nine Chromium frame rounds
with an A/A pair, at host load 20 to 60. Each slower case was re-run in
alternating rounds and counted in callgrind instructions.

Fixed in [moq-dev/moq#6784](https://github.com/moq-dev/moq/pull/6784):

- The MKV and FLV exports rebuilt the jitter clock's expected track set on
  every frame; they now set it once per catalog update.
  `container-export/mkv/16x64` went from 27.1M to 17.6M instructions (2.27x to
  1.47x release, flat across the sweep), `flv/16x64` 14.5M to 12.9M.
- The TS export and its T-STD schedule hash PIDs with FxHash:
  `container-export/ts/16x64` 30.8M to 25.5M (2.70x to 2.24x).
- Benches that broke since release now run: the moq-mux catalog bench caps its
  renditions, the relay auth bench reads its observed state directly, moq-stats
  `producer_tick` skips tiers above one at 50,000 paths, and `bench/run.sh`
  skips targets that are not Criterion.

Accepted, flat per operation:

- e2ee `protect` is 1.5x release on grouped frames and 1.21x on datagrams. The
  crypto is unchanged; the rest is the per-group model cost accepted above.
  Both branches expand the AES-GCM key on every frame, about a quarter of
  `protect`, which the [performance audit](/quest/m2/audit-perf.md) owns.
- gst `moqsink/eos` is +12% instructions per event, from
  [#5512](https://github.com/moq-dev/moq/pull/5512) and the gstreamer-rs 0.25
  bump in [#6169](https://github.com/moq-dev/moq/pull/6169).
- `group_write_frames` (1.67x) and `track_stamp_groups/sparse` (1.48x, down
  from 4.4x) are the per-frame and per-group costs accepted above, as are the
  session join, burst, and delivery families (1.2x to 1.76x).
  `session_dash_relays` is the same per-track cost: 1.50x instructions at 768
  tracks over 2 or 3 relays, and 0.63x over 33 relays, so it falls with relay
  count. The import fixed costs above (`annexb-import`, `cmaf-import` at one
  frame) and the IETF `/fetch` delivery rows also reappear unchanged.
- The TS export still grows with track count (1.50x at `1x1` to 2.24x at
  `16x64`). The T-STD schedule scans and sorts every queued unit per PCR slot,
  which the [performance audit](/quest/m2/audit-perf.md) owns.

Noise: `frame_recv/upfront/size=262144/streams=64` is 3 to 4x in wall time at
0.96x instructions. Its 1.6M page faults (against 192k) are glibc returning
the freed 256 KiB buffers between iterations; with
`GLIBC_TUNABLES=glibc.malloc.mmap_threshold=1048576:glibc.malloc.trim_threshold=1073741824`
both sides fault 4.6k times and time the same, and the relay runs jemalloc.
`session_idle` IETF at 1024 sessions, `origin/resume_overlap`,
`origin/retire`, `origin/reprice_duplicate` (0.55x instructions), kio
`contended_notify`, and moq-stats `producer/merge` are at parity in
alternating rounds or instructions.

Relay load: video used 7.8% less relay CPU with the same delivery. Fanout used
16.4% more, but its paired deltas were -1.5%, +16.4%, and +26.1% (MAD 9.7%)
at load 25, and the 2026-10-21 run measured 12% less, so it needs a quiet
re-run. Browser frames: lite rows 1.09 to 1.14x and IETF 0.95x, the same as
`main` at the 2026-10-23 decision (1.13x lite).

Decided 2026-10-25 (maintainer), in
[moq-dev/moq#6784](https://github.com/moq-dev/moq/pull/6784):

- The sparse read slope from
  [dynamic range requests](https://github.com/moq-dev/moq/pull/6171) blocks
  the cut; [moq-dev/moq#6921](https://github.com/moq-dev/moq/pull/6921)
  flattened it.
- The TS schedule's growth is left to the
  [performance audit](/quest/m2/audit-perf.md); it does not block the cut.
- Now that the sparse fix has landed, re-run the relay fanout pairing on a
  quiet host, recording `/proc/loadavg`, with
  `BENCH_ARTIFACTS=<scratch> just bench origin/release` (it also re-checks
  the sparse rows). Fix it if the relay
  CPU still rises with consistent sign; otherwise this epic is done.

### Re-run (2026-10-30)

`release` (`de0a2b0534`) against `main` (`7c8e255304`). The host was shared
with other agents: 1-minute load 11 to 29 on 24 cores. Rather than the full
`just bench origin/release`, this run paired only the relay workloads (the
tail of `bench/run.sh`, seven alternating rounds) and `track_sparse_read`
(three alternating rounds). Release's `track` bench has no `async` feature, so
`run.sh --criterion` cannot build both sides with one set of arguments.

Relay fanout is bimodal on `main`. Release's relay used 0.062 to 0.082 cores
in all seven rounds. Main's used 0.062 to 0.072 in three rounds and 0.157 to
0.241 in four, with the same delivery (640 frames/s, p99 within 2%). The
comparator calls the sign mixed (paired deltas -15% to +218%, median +119%),
but the high runs share a mechanism. In them, 21 to 24 Tokio workers are busy
and the relay makes 9,400 to 12,600 voluntary context switches/s, against 2
to 6 busy workers and 400 to 1,300 switches/s otherwise. Release never
exceeded 4 busy workers or 2,800 switches/s. Eight CPU-profiled runs of
`main` reproduced it in five. In a high run, 54% of samples end in a syscall
(3 of 227 in a low run). Most are futex waits on the track's shared
`kio::Lock`, taken by every subscriber's `read` in
`poll_recv_next → poll_peek_group` and
`poll_forward_drop → poll_drop`, plus the wakes that follow. When the 64
subscriber tasks land on many workers, every frame makes them contend that
one lock; packed onto a few workers, they never contend. Video used 9.7% less
CPU (MAD 7.1%), with 1.4% more frames delivered.

`track_sparse_read/timed` is flat now that
[moq-dev/moq#6921](https://github.com/moq-dev/moq/pull/6921) has landed:
release 2.42, 2.49, 2.64 us and main 5.33, 5.98, 6.42 us at 16, 256, 4096
gaps (medians of three rounds). From 16 to 4096 gaps, main grows 20% and
release 9%. The 2.2 to 2.4x gap is per iteration, not per gap. Each iteration
creates, writes, finishes, and receives two groups, so this is the per-group
model cost accepted above.

Decided 2026-10-30 (maintainer), in
[moq-dev/moq#7299](https://github.com/moq-dev/moq/pull/7299):

- The sparse rows are accepted as that per-group model cost.
- The fanout mode is accidental and is fixed before the cut, in
  [Fanout lock contention](/quest/m1/release-perf/fanout-lock.md).

## Required

- [Fanout lock contention](/quest/m1/release-perf/fanout-lock.md) - relay fanout subscribers stop contending one track lock, so main's relay CPU matches release's in every round

## Related

- [Performance audit](/quest/m2/audit-perf.md) - compares nightly to nightly; this epic compares main to release
- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - noise floor of the paired nightly-to-nightly comparison on one runner
