# [M] Compare current Opus implementations

## Goal

Determine whether an optional newer Opus backend improves quality or CPU enough
to justify its build and maintenance cost while retaining the simple Rust path.

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

Retain fixtures and repeatable measurements in the existing CI/nightly audio
harness. Public API and wire: no change for the study; validate compatibility
before a separately scoped backend implementation.

## Related

- [Audio configuration](/quest/main/audio-config.md) - stable settings and backend-selection boundary
- [Audio quality](/quest/next/audio-quality-harness/README.md) - shared measurement infrastructure
