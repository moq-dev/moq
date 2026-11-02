# [M] Relay fanout subscribers stop contending one track lock

## Goal

The relay's `fanout` workload (`bench/relay.sh`, 64 subscribers of one track)
uses no more relay CPU on `main` than on `release` in every alternating
round, with the same delivery. Subscribers of one track no longer serialize
on the track's shared `kio::Lock` for every frame, so spreading their tasks
across Tokio workers no longer multiplies the relay's CPU.

## Plan

Found by the 2026-10-30 re-run in the
[release-perf epic](/quest/m1/release-perf/README.md)
([moq-dev/moq#7299](https://github.com/moq-dev/moq/pull/7299)), `release`
`de0a2b0534` against `main` `7c8e255304`. Decided 2026-10-30 (maintainer):
this is accidental cost, fixed before the cut.

What the profile found:

- `main`'s relay is bimodal under `fanout`. Release used 0.062 to 0.082
  cores in all seven rounds; main used 0.062 to 0.072 in three and 0.157 to
  0.241 in four, at the same 640 frames/s and p99.
- In the high mode 21 to 24 Tokio workers are busy and the relay makes 9,400
  to 12,600 voluntary context switches/s, against 2 to 6 workers and 400 to
  1,300 switches/s in the low mode. Release never exceeded 4 busy workers or
  2,800 switches/s. Eight CPU-profiled runs of `main` (frame pointers, the
  default shared runtime) reproduced the high mode in five.
- In a high run 54% of samples end in a syscall (3 of 227 in a low run),
  mostly futex waits on the track's shared `kio::Lock` and the wakes after
  them. Every subscriber's read takes it, in the lite publisher's
  `poll_recv_next` → `poll_group` / `poll_peek_group` and `poll_forward_drop`
  → `poll_drop` (`rs/moq-net/src/lite/publisher.rs`), which reach the track
  state through `rs/moq-net/src/model/resume.rs` and
  `rs/moq-net/src/model/track.rs`.
- Packed onto a few workers, the 64 tasks never contend. Spread over many,
  each frame wakes them all onto one mutex.

Approach:

- First find what `release`'s read path did differently, since release ran in
  the same rounds and never spread. Compare how many times one subscriber's
  wake takes the track lock on each branch.
- Then cut the per-wake lock work: take the lock once per wake rather than
  once per poll step, or keep per-reader state outside the producer's mutex
  so a reader only locks when a new group or drop is published.
- Add a Criterion case that reproduces the contention without the relay
  harness (many readers of one track on a multi-threaded runtime), and keep
  it in the release pairing.
- Verify with seven alternating `fanout` rounds against `origin/release`,
  recording `/proc/loadavg`, busy workers, and voluntary context switches
  per run; a quiet host is preferred. `release` has no `listen.tcp.tls`, so a
  `just bench origin/release` pairing passes `"quic ws wss tcp"` (or just
  `quic`). `just bench-profile` cannot yet run the default shared runtime,
  where the mode shows up.

Public API: none expected. Wire: none.

## Related

- [Log lock profile](/quest/m1/unbounded-groups/log-lock-profile.md) - lock contention profiling of `GroupState`, not the track lock
