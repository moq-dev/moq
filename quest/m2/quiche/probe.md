# [L] Probe by early retransmission

## Goal

Egress discovers bandwidth headroom by retransmitting in-flight data early
instead of sending PADDING: probe bytes are useful if the original was lost
and cost no more than padding if it was not. The raised delivery-rate
estimate flows through the EXISTING plumbing - the transport's
`estimated_send_rate` -> MoQ PROBE -> publisher rate adaptation - so no new
consumer is built.

## Plan

- Fork-side work lands in the moq-dev/quiche repository; this quest covers the
  moq-side adoption in this repo plus the fork PRs.
- Today the estimate only reflects what congestion control already sends: an
  encoder cannot learn there is headroom without someone spending bytes above
  the current rate, and padding-based probing (the WebRTC approach) spends
  them on garbage. Duplicating the most-recent unacked packets spends the
  same bytes on redundancy.
- Lands in the fork's recovery/pacing layer as an opt-in config, driven by
  demand: probe only while the application is consuming the bandwidth
  estimate, mirroring how moq-net's bandwidth sampler in `session.rs` gates
  on `poll_used`. Idle connections never probe.
- Accounting must stay truthful, and it is two-sided: a probe retransmission
  that gets acked alongside the original must not count as loss, and probe
  bytes must not double-count as new application delivery. But an
  application-limited sender's delivery rate is capped at the encoder rate by
  definition, so probe acks MUST feed a wire-capacity sample distinct from
  goodput, or the estimate can never rise above the encoder rate and the
  probing is pointless. Getting this arithmetic right IS most of the quest.
- Open questions for the implementation, not blockers: probe cadence and step
  size (multiplicative like BWE probing, or paced ramp), interaction with
  BBR2's own PROBE_BW cycle, and whether group-tail packets (about to be
  reset by `max_age` eviction) are excluded as probe payload.

## Related

- [FEC experiment](/quest/m3/quiche-fec.md) - early retransmission is a
  repetition code; both spend the same spare-bandwidth budget on redundancy
- [GCC egress experiment](/quest/m3/quiche-gcc.md) - a delay-based controller
  changes what "headroom" means; the probing design should not assume BBR2
