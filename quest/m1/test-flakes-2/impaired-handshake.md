# [S] Impaired drill clients connect reliably

## Goal

The impaired drills (`bursts_cross_a_cluster::impaired`,
`bursts_cross_a_flapping_peer::impaired`, and
`interrupted_publisher_republishes_new_content::impaired` in
`rs/moq-relay/tests/drills.rs`) never fail while connecting their publisher or
subscriber.

## Plan

Qlogs of failing runs (2026-10-09) show two causes. Both come from the
drills' 2s client idle timeout meeting a backed-off PTO during the handshake.

1. **A lost Finished waits on the Initial backoff.** RFC 9002 A.11 resets
   `pto_count` when Initial or Handshake keys are discarded. noq and quinn do
   not, and a client never resets it on an Initial ACK. After five Initial
   PTOs, a client armed its Handshake PTO 2.3s out. Its Finished was lost, the
   server cannot read 1-RTT without it, and the server idled out first. The
   client saw `reset by peer`. Fixed in `moq-quic` and in
   [moq-dev/noq#32](https://github.com/moq-dev/noq/pull/32); the drills pick it
   up once that ships in a `moq-noq-proto` release and the pin moves to it.
2. **Two Initial flights per idle window.** Before an RTT sample the base PTO
   is 999ms, so the idle timer is armed at `max(2s, 3 * 999ms)` = 2997ms. The
   probes go at 0 and 999ms. The second probe is due at 2997ms plus timer slop,
   so it always lands a millisecond or two after the idle deadline. If neither
   flight draws a reply, the client fails `Noq(Connection("timed out"))`. On
   `bursty()` the second ClientHello datagram (it spans two, with the ML-KEM
   key share) always overflows the 50ms queue, so each flight relies on the
   server's ACK getting through. This was 3 of 300 cluster runs and 1 of 480
   republish runs, and it is the quest's original failure.

Cause 2 needs a decision, since it changes QUIC stack behavior:

- A handshake idle timeout separate from the idle timeout, as msquic
  (`HandshakeIdleTimeoutMs`, 10s) and Chromium do. Dead-peer detection on an
  established connection is unchanged. This needs a new `TransportConfig` field
  in `moq-quic`, and in moq-dev/noq too if the switch is not close.
- Cap the handshake probe interval so several probes fit in the idle window,
  as picoquic does. This needs no API, but it changes the PTO deadline
  computation, which has to stay monotonic in `pto_count`.
- Floor the idle timer at 3x the backed-off PTO. This slows dead-peer
  detection whenever probes are outstanding, which works against the drills'
  killed-relay budget.

Measure with a loop of the impaired drills under load. A temporary
`quic.qlog` from an env var in `quic()` and `relay_config` captures a failing
run's qlog (needs the `qlog` feature). A missing server qlog means no client
Initial ever arrived.

Public API: none so far. Wire: none.

## Related

- [Switch](/quest/m1/quic/fork/switch.md) - moves the runtimes from noq to `moq-quic`, which already carries the cause 1 fix and is where a cause 2 fix may land instead
