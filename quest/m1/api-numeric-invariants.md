# [M] Make accepted numeric API values encodable

## Goal

Every accepted timescale and track delivery property survives encoding without
truncation, precision loss, or a refusal by the matching reader. Reject invalid
input at the public boundary, before publishing partially written metadata.

## Plan

Audit evidence at dev `e2350b39a` (2026-09-12):

- `rs/moq-net/src/model/time.rs:73` implements `From<NonZero<u64>>` without
  the varint bound enforced by `Timescale::new`. `NonZero::new(u64::MAX)`
  therefore constructs a scale that `lite/track.rs:86` cannot encode.
- `js/net/src/time.ts:83` uses `Number.isInteger`, accepting unsafe integers.
  Executed against the current source: `Timescale(2 ** 53)`,
  `Timescale(2 ** 62)`, and `Timescale(1e100)` all succeed.
- `js/net/src/track.ts:80-92` accepts unchecked priority/timescale and finite
  ages beyond the writer's safe integer range. `stream.ts:448` warns and
  continues in `u53`, while the matching reader refuses unsafe values.
  `Writer.u8` also wraps an out-of-range priority through `setUint8`.

Replace the infallible Rust conversion with a checked conversion, routing all
construction through the invariant. Validate JavaScript safe integer ranges,
priority, and encoded duration bounds at their public entry points; writers
must also refuse malformed input before emitting bytes. Preserve intentional
unit conversion and rounding, documenting the supported precision. Do not
broaden this into a new numeric framework or force JS numbers to represent
the entire Rust u64 domain.

Add boundary regressions to the existing Rust model/codec and JS time/track/
writer suites: zero, negative, fractional, NaN/infinity, safe maximum and one
past it, varint maximum and one past it, and priority 255/256. Prove valid
metadata round-trips and rejected writes emit no malformed field.

Public API: the Rust `From` removal is breaking; JS invalid inputs now throw.
Wire: no format change, only refusal of values outside the existing domain.
Run `just check`, `just test`, and `just test smoke-full`.
