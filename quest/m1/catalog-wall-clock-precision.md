# [XS] Exact catalog wall clock

## Goal

`hang::catalog::Clock::wall_clock(pts)` returns the exact instant
`wall + pts` names, down to the nanosecond, instead of rounding down to the
millisecond. Today it computes
`unix_millis = MOQ_EPOCH_UNIX_MILLIS + total * 1000 / scale` and returns
`UNIX_EPOCH + Duration::from_millis(unix_millis)`, dropping up to 999 µs of a
microsecond wall. `moq_mux::Clock::wall_clock` delegates to it and documents
"Refuses an unrepresentable result rather than truncating it", which this
contradicts. Any caller turning a pts into a wall time loses the
sub-millisecond part, and one that rebuilds a mapping from the result
(`Clock::at(…, wall_clock(ZERO))`) re-advertises a wall up to 999 µs early.

Non-goal: JS `wallClockTime`, which returns a millisecond `Date` by nature.

## Plan

Decided 2026-10-12: split from
[adopting a catalog clock](/quest/m1/catalog-clock-adopt.md); the two are
independent.

Combine the wall and pts as exact rationals before rounding anything:
today `pts.as_scale(wall.scale())` floors first, so a 90 kHz tick on a
microsecond wall loses 111 ns and a 1 µs pts on a millisecond wall vanishes,
and splitting that `total` into seconds and nanos cannot recover it. Sum
`wall.value() * pts_scale + pts.value() * wall_scale` over
`wall_scale * pts_scale` with checked `u128` arithmetic, keep the
JSON-safe-range check on the wall's own units, and floor once when building
the `Duration` from seconds plus nanos (keeping the existing
`TimestampOverflow` for a wall `SystemTime` cannot reach). Floor an
instant finer than a nanosecond (decided 2026-10-12: refusing it would break
ordinary 48 kHz and 90 kHz pts), and say so in both `wall_clock` docs
instead of "rather than truncating".

Tests: a whole-microsecond `wall` with a sub-millisecond part round-trips,
`moq_mux::Clock::at(epoch, wall)` → `wall_clock(ZERO)` gives back `wall`;
the existing cross-timescale cases still hold; fractional cross-timescale
cases keep their precision (one 90 kHz tick on a microsecond wall is
11,111 ns, a 1 µs pts on a millisecond wall adds 1,000 ns) and floor only at
the nanosecond.

Public API: none (behavior fix). Wire: none.

## Related

- [The catalog root clock is the producer clock](/quest/m1/catalog-clock-adopt.md) - builds the same mapping in the other direction
