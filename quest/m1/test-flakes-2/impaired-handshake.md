# [XS] Impaired drill clients connect reliably

## Goal

The impaired drills (`bursts_cross_a_cluster::impaired`,
`bursts_cross_a_flapping_peer::impaired`, and
`interrupted_publisher_republishes_new_content::impaired` in
`rs/moq-relay/tests/drills.rs`) never fail while connecting their publisher or
subscriber.

## Plan

Both causes are fixed in the QUIC stack, in `moq-quic` (#5163) and in
moq-dev/noq ([#32](https://github.com/moq-dev/noq/pull/32) and
[#34](https://github.com/moq-dev/noq/pull/34), stacked). The drills still run
on `moq-noq`, and [the switch](/quest/m1/quic/fork/switch.md) is not started,
so what remains is to bump the `moq-noq-proto` pin to a release carrying both
noq PRs. Then loop the impaired drills under load (four to eight in parallel,
a few hundred runs each) and confirm that no connect fails.

The causes, from qlogs of failing runs (2026-10-09). Both are the drills' 2s
client idle timeout meeting a backed-off PTO during the handshake.

1. **A lost Finished waited on the Initial backoff** (`reset by peer`).
   RFC 9002 A.11 resets `pto_count` when Initial or Handshake keys are
   discarded; noq and quinn did not. The server, which cannot read 1-RTT
   before the Finished, idled out first. Fix: reset `pto_count` in
   `discard_space`.
2. **Two Initial flights per idle window** (`timed out`). The idle timer was
   armed at `max(2s, 3 * 999ms)` = 2997ms, and the second probe is due at
   2997ms plus timer slop. Decided 2026-10-09: add a separate
   `handshake_idle_timeout` (default 10s, as msquic does) that governs until
   the handshake completes. Dead-peer detection on established connections is
   unchanged. The rejected options were capping the handshake probe interval
   (it touches the PTO deadline's monotonic invariant) and flooring the idle
   timer at the backed-off PTO (it slows dead-peer detection).

With the bump, a handshake to an unreachable relay waits 10s instead of about
3s. Check that the crash drill's reconnect loop (`dial()`'s 5s backoff
timeout) still holds.

Public API: none in this repo; `moq-quic` and `moq-noq-proto` gain
`TransportConfig::handshake_idle_timeout`. Wire: none.

## Related

- [Switch](/quest/m1/quic/fork/switch.md) - moves the runtimes from noq to `moq-quic`, which already carries both fixes
