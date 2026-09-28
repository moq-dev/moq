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

moq-dev/noq#5 added `Controller::on_app_limited` for this and shipped in
moq-noq 1.3.1; check what it leaves open before writing more code.

The same defect is the likely cause of
[#4219](https://github.com/moq-dev/moq/issues/4219): after a long
keep-alive-only idle, a BBRv3 (`delay`) sender paced its next burst at one
packet per RTT instead of near the bandwidth it had learned. The reporter ran
1.3.0 and nobody has reproduced it on either version. Add a virtual-time
transport test: learn the bandwidth, idle on keep-alives for about five
minutes, send 250 KB, and assert the pacing rate stays at or above about 0.9x
the earlier max bandwidth. It should fail on 1.3.0. If it still stalls on the
fix, find the remaining cause (a stale `bw_shortterm` or a ProbeRTT effect
after idle). Dropping the estimate after a long idle is a policy change for
the m2 study, not this quest. The `iroh` feature uses upstream noq, which
lacks the fix; offering it there belongs to the upstream quest.

## Closes

- [#4219](https://github.com/moq-dev/moq/issues/4219) - the first send after an idle period is paced at a trickle

## Related

- [Finish each BBR ACK sample](/quest/m1/quic/bbr-ack-sampling.md) - a separate ordering defect in the same callback lifecycle
- [Release BBR fixes](/quest/m1/quic/bbr-release.md) - deliver correct labels before policy experiments
- [Upstream the fork](/quest/m1/quic/upstream.md) - offer the general fix upstream
