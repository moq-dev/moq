# [M] Mark application starvation before the next BBR send

## Goal

Packets sent after application starvation carry the correct historical
application-limited label, even when no ACK arrives during the idle gap.
The bandwidth model cannot mistake a source-limited sample for capacity.

## Plan

In noq `1a26a8b064d21e316fe6769f068617975bd8a27b`, an empty unblocked
[transmit poll](https://github.com/n0-computer/noq/blob/1a26a8b064d21e316fe6769f068617975bd8a27b/noq-proto/src/connection/mod.rs#L1337) records starvation, but BBR receives the marker only in
[on_end_acks](https://github.com/n0-computer/noq/blob/1a26a8b064d21e316fe6769f068617975bd8a27b/noq-proto/src/congestion/bbr3/mod.rs#L1732).
A resumed send can be stamped before that notification. Google's
[QUICHE BBR3](https://github.com/google/quiche/blob/535a2730e77d47e0dc03746555cc9c34b17bc9e9/quiche/quic/core/congestion_control/bbr3_sender.cc#L505) notifies its sampler immediately when application limited.

Reproduce through the transport boundary: send and ACK one 1200-byte packet
with a 10-ms RTT, run an empty unblocked transmit poll, wait until 30 ms to
send another packet, then ACK it 10 ms later. There is no intervening ACK
to notify the controller of starvation. The existing public-callback
reproduction yields a non-limited sample; preserve a failing regression
without privately seeding the sampler marker. This establishes a label bug,
not a measured throughput regression.

Communicate starvation before subsequent sends, preserving the delivery
boundary that ends the sampler's limited phase. Distinguish producer
starvation from cwnd, pacing, anti-amplification, receiver credit, and local
buffer limits; do not silently change receiver-limited policy. Cover streams,
datagrams, resumed backlog, repeated empty polls, and ACK batching. Coordinate
any controller event changes with the packet identity and ACK sampling fixes.
Keep state private where possible; document any public Controller change and
its consumers. No wire change is intended. Wire regressions into fork CI.

## Required

- [Fork noq](/quest/next/quic/fork.md) - fixes and CI regressions land in moq-dev/noq

## Related

- [Finish each BBR ACK sample](/quest/next/quic/bbr-ack-sampling.md) - a separate ordering defect in the same callback lifecycle
- [Release BBR fixes](/quest/next/quic/bbr-release.md) - deliver correct labels before policy experiments
- [Upstream the fork](/quest/next/quic/upstream.md) - offer the general fix upstream
