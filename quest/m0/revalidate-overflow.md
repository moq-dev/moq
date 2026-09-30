# [XS] Auth durations cannot overflow a deadline

## Goal

No auth-server or token duration can panic the relay. Today
`moq-auth/src/client.rs` computes `Instant::now() + cadence` for the
`revalidate` value, and `Grant::deadline` adds a token's `exp` the same way.
A value near `u64::MAX` seconds overflows, and `panic = "abort"` ends the
process.

## Plan

- Use `checked_add`. `Grant::validate` refuses a `revalidate` longer than the
  grant's expiry, and treats an `exp` beyond `Instant`'s range as no expiry.
- Unit tests with `u64::MAX` revalidate and exp.
- Grep the workspace for other `Instant + Duration` on peer- or
  server-supplied values.

Public API: none. Wire: none.
