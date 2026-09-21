# [M] Decide a tested Opus loss-recovery policy

## Goal

Decide whether a concrete MoQ audio consumer benefits from in-band Opus FEC,
with a tested loss/latency policy before exposing a replacement public option.
This does not block 0.1.

## Plan

The old boolean supplied no expected-loss percentage and our decoder never
requested FEC recovery. Treat source loss, late-group abandonment, and DTX as
different cases. Define sequencing, expected loss, playout lookahead, recovery
versus concealment, and behavior when the next packet is unavailable.

Use deterministic dropped-packet fixtures to prove redundancy is emitted and
used, compare audible quality and delay against concealment, and name the
transport scenario where it helps. End with a measured go/no-go and a small
additive policy if justified; do not reintroduce an enable flag tested only by
reading the codec's control value. Implementation fixtures belong in CI.

Public API: no change during the study; any later policy must fit the extensible
audio settings. Existing wire compatibility must be demonstrated.

## Related

- [Audio quality](/quest/next/audio-quality-harness/README.md) - quality and latency measurements
