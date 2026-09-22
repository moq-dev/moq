# [M] Finish each BBR ACK sample before using it

## Goal

Each ACK updates the BBR model and control parameters from a completed,
consistent sample. Bandwidth, delivered bytes, application-limited status,
and inflight describe the same observation.

## Plan

In [the audited callback order](https://github.com/n0-computer/noq/blob/1a26a8b064d21e316fe6769f068617975bd8a27b/noq-proto/src/congestion/bbr3/mod.rs#L1656), on_ack updates the model per
packet before on_end_acks computes delivery_rate and delivered. Current
packet metadata is combined with the preceding ACK's values. Follow the
ordering in [draft section 5.2.3](https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.html#section-5.2.3) and
[Google QUICHE](https://github.com/google/quiche/blob/535a2730e77d47e0dc03746555cc9c34b17bc9e9/quiche/quic/core/congestion_control/bbr2_sender.cc#L271), adapted to noq's event boundary.

Reproduce both failures in the fork: the first completed 1200-byte/10-ms
sample leaves max_bw at zero; and, with an aged prior bandwidth maximum, a
120 KB/s application-limited sample is admitted under the next burst's
non-limited metadata even though that burst delivers 1.2 MB/s. Verify fresh
samples are consumed once, with the correct labels and post-ACK inflight.

Cover batched ACKs, reordered ACKs, invalid/too-short intervals, loss-only
events, and idle restart. Preserve the transport's RTT and loss event
ordering while fixing the source of stale state. Reuse the existing
controller simulations and add regressions to the fork's CI. Coordinate
with packet identity work if the callback contract changes; settle the API
with the maintainer and update its consumers and docs in the same PR.

## Related

- [Release BBR fixes](/quest/next/quic/bbr-release.md) - deliver the corrected controller to MoQ
- [Upstream the fork](/quest/next/quic/upstream.md) - offer general fixes upstream
- [BBR3 app-limited](/quest/future/quic-bbr-app-limited.md) - measure the corrected controller on media traffic
- [Packet identity](/quest/next/quic/bbr-packet-identity.md) - shares the controller boundary
