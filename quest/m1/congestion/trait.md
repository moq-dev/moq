# [XL] Portable controller trait

## Goal

`moq-congestion`'s trait depends on no transport's types, so any QUIC stack
can drive BBR3 and Cubic with a thin adapter. `moq-quic` runs the redesigned
controllers with no throughput, latency, or CPU regression on its shaped
bufferbloat and sim benchmarks.

## Plan

Decided with the maintainer on 2026-10-23:

- **s2n-quic shape** ([s2n-quic-core's congestion_controller.rs](https://github.com/aws/s2n-quic/blob/main/quic/s2n-quic-core/src/recovery/congestion_controller.rs)):
  the controller returns per-packet state when a packet is sent, and the
  transport stores it in its sent-packet record and hands it back on ACK,
  loss, or discard. BBR3 then drops its own packet map.
- **That state is one opaque crate struct** with private fields, not an
  associated type, so the trait stays object-safe (`Box<dyn>`) and generics
  don't spread through `moq-quic`, `moq-tokio`, `moq-uring`, and `moq-ffi`.
  It starts with delivery-rate sampling state (draft-ietf-ccwg-bbr, Linux
  `tcp_rate.c`), and rate sampling becomes a shared crate component that
  later controllers reuse. Keep it small and `Copy`; it lives in every sent
  packet.
- **The controller owns pacing.** The crate ships the pacer, and the trait
  exposes when the next packet may leave and how large a burst may be.
  `moq-quic` deletes `connection/pacing.rs`. A transport that keeps its own
  pacer (noq) reads the rate instead.
- **Crate-owned inputs.** An event struct replaces the interim `RttEstimator`
  snapshot and QUIC-shaped `SpaceId` (or packet identity becomes opaque), and time
  stays `moq_clock::Instant`.
- **Exhaustive structs, explicit breaks.** Event structs that transports
  build are not `#[non_exhaustive]`: a new field breaks the transport's
  literal, so noq and quinn must wire each new signal in a 0.x minor bump
  instead of silently dropping it. Transports never re-export crate types.
- **Signals enter with their consumer.** Ship what BBR3, Cubic, and Unlimited
  consume. For each follower, sketch here the field it would add and confirm
  it is additive: receive timestamps per acked packet (GCC, SCReAM, BBR3
  delay bound), the controller's ECT codepoint choice and CE counts (L4S),
  seed state at construction (careful resume), and the probe signal
  native probing reads (`Controller::probes` and
  `ControllerMetrics::probe_inflight`, from [moq-dev/moq#6635](https://github.com/moq-dev/moq/pull/6635)).

Measure the per-packet cost (the state struct, the removed packet map, the
pacer move) with a benchmark, alongside the existing cleanup benchmark, and
run the bufferbloat lanes in `test/ts/README.md` before and after.
