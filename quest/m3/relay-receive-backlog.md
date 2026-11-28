# [L] The relay keeps draining its socket through a join storm

## Goal

`chat`'s 2,000 viewers join `main`'s relay without it losing a group: over at
least ten alternating rounds of `just bench-relay origin/release quic` on a
shared host, no `main` round loses more groups than `release`'s worst, and its
UDP receive queue fills the 16 MiB buffer no more often than `release`'s.

## Plan

Found by [Chat startup loss](/.github/release-perf.md#chat-startup-loss-2026-11-27).
Decided 2026-11-28 (maintainer, on
[moq-dev/moq#9825](https://github.com/moq-dev/moq/pull/9825)): the 0.22.0 cut
accepts this loss, and the quest stays in m3. Rejected: holding the cut on it in
m0, and adding a cut gate first.

Facts:

- In every round that lost groups, the relay's UDP receive queue
  (`udp_rx_queue_bytes` in `host.jsonl`) was 6.4 MiB or deeper, and in most
  it filled the 16 MiB buffer and the kernel dropped datagrams. `main` filled
  it in 8 of 14 rounds, `release` in 1 of 13, on `fedora` at load 29 to 70.
- The publisher's group streams reached the relay 2.0 to 2.7 s after the
  publisher opened them, so some arrived past the viewers' 2 s budget and
  others never arrived. No viewer was sent a drop for the groups the relay
  never received.
- The relay used less CPU in the lossy rounds (1.9 to 2.8 cores) than in the
  clean ones (4.1 to 4.2), and no thread was pegged: it stops draining the
  socket rather than running out of CPU.
- With 8 QUIC shards (`tokio-workers`, one socket each) neither relay queued
  more than 131 KiB, but at load 15, so it does not yet separate the socket
  from the host.

Start by profiling the relay through the ramp of a lossy round, on the
default shared runtime that `just bench-relay` runs. `just bench-profile`
defaults to `tokio-workers` and captures from second 5, after the 2 s ramp
(`bench/relay.sh`), so either start its capture with the relay or record
`perf` on the relay's PID from its start. Look at CPU stacks and the
scheduler's view of the endpoint task: how often it runs, and for how long
against its 50 µs receive budget (`RECV_TIME_BOUND` in
`rs/moq-tokio/src/quic/driver/mod.rs`). Compare with `release`'s `noq`
endpoint on the same host. Then rerun shards at the shared runtime's load. The scratch
probe that timed each group across the hops is in the description of
[moq-dev/moq#9825](https://github.com/moq-dev/moq/pull/9825); reapply it to
confirm a fix.

Also check why a group the publisher gave up while it waited never reached
viewers as a drop: whether the publisher sent one and the relay dropped it, or
sent none.

## Related

- [Track lock passes](/quest/m3/relay-track-lock.md) - `main`'s extra relay CPU per frame in the same workload
- [QUIC connection lock and packet dispatch profile](/quest/m4/perf/quic-lock-contention.md) - the endpoint-to-connection packet hop this backlog sits behind
