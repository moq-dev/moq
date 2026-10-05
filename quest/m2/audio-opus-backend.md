# [M] Compare Opus implementations and loss recovery

## Goal

Determine whether an optional newer Opus backend improves quality, CPU, or
loss recovery enough to justify its build and maintenance cost while retaining
the simple Rust path, and settle a tested loss-recovery policy.

## Plan

The manifest describes unsafe-libopus as a Rust port of 1.3.1. Compare it with
a currently maintained upstream implementation, including
[Opus 1.6](https://opus-codec.org/demo/opus-1.6/), rather than treating old
unmeasured percentage claims as a decision. Verify the actual versions at
implementation time.

Measure relevant voice/music rates, frame sizes, DTX, loss behavior, startup,
CPU, and package/toolchain cost. Keep native compilation optional and use the
existing private backend seam; do not add another public configuration surface
just to expose one implementation's controls. A no-go is a valid outcome.

### Loss recovery

Decided in the 2026-09-30 audit: the loss-recovery policy study folds in here,
because the options depend on the backend. DRED and deep PLC need libopus 1.5+,
while the current 1.3.1 port only offers in-band LBRR FEC and classic
concealment.

Decide whether a concrete MoQ audio consumer benefits from FEC, DRED, or deep
PLC over concealment. The old FEC boolean supplied no expected-loss percentage
and our decoder never requested FEC recovery. Treat source loss, late-group
abandonment, and DTX as different cases. Define sequencing, expected loss,
playout lookahead, recovery versus concealment, and behavior when the next
packet is unavailable. Use deterministic dropped-packet fixtures to prove
redundancy is emitted and used, compare audible quality and delay, and name
the transport scenario where it helps. Any resulting policy is small and
additive on the extensible audio settings; do not reintroduce an enable flag
tested only by reading the codec's control value.

Retain fixtures and repeatable measurements in the existing CI/nightly audio
harness. Public API and wire: no change for the study; validate compatibility
before a separately scoped backend implementation.
