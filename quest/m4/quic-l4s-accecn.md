# [M] Add L4S and AccECN support (ECT(1) and per-ACK CE counts) to moq-quic and moq-congestion

## Goal

`moq-quic` can mark outgoing packets with `ECT(1)` (L4S) in addition to `ECT(0)`, validates `ECT(1)` feedback without tripping `"ECN corruption"`, and passes exact per-ACK newly-marked CE counts to `moq_congestion::Controller::on_ce` so BBRv3 / SCReAMv2 can react proportionally to L4S ECN marking for sub-millisecond queuing on L4S-capable paths.

## Plan

Decided 2026-11-26 (maintainer, `/quest-plan` of the `moq-quic` audit): ✅ include L4S / AccECN support among the planned net-new features (rejected: RACK-TLP & BDP receive-window auto-tuning; 0-RTT session resumption; leaving L4S out). Joins `m4` as a real-time transport feature after the 0.22 cut.

Current limitations found in the audit:

- `PacketSpace::detect_ecn` (`rs/moq-quic/src/connection/spaces.rs:194-220`) rejects `ect1_increase != 0` as `"ECN corruption"` and only supports `ECT(0)` marking.
- `moq_congestion::Ce` (`rs/moq-congestion/src/lib.rs:238-243`) only carries `{ now, sent }` as a boolean congestion trigger without `ce_increase` or `acked_ect` counts, preventing proportional L4S / Prague / SCReAMv2 / BBRv3 ECN-alpha rate control.

Guidance:

- Allow the congestion controller (or transport config) to choose `ECT(0)` vs `ECT(1)` marking per path, and update `detect_ecn` to validate against the codepoint actually sent on that path (falling back cleanly to `NotEct` if `ECT(1)` is bleached or corrupted).
- Pass `ce_packets` and `acked_packets` (or `ect1`/`ce` deltas) in `congestion::Ce` so controllers can compute L4S marking fraction `alpha`.
- Extend `moq-quic`'s `sim::Link` with an L4S DualQ / shallow-threshold CE marker and test BBRv3 and SCReAMv2 queuing latency under L4S vs classic bufferbloat.

Public API: additive (optional ECN/L4S mode on `#[non_exhaustive]` config; `moq-congestion` `Ce` fields). Wire: none (standard RFC 9000 `ACK_ECN` + RFC 9330/9331 `ECT(1)`).

## Related

- [Realtime congestion control](/quest/m5/quic-realtime.md) - SCReAMv2 and BBR3 delay-bound candidates that benefit from L4S CE fractions
