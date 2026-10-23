# [M] moq-net internals after transport erasure

## Goal

With the transport boxed, the lite publisher stops cloning the session and
group per group, and the lite and IETF internals lose the transport type
parameter they keep only for test mocks. A `just bench-runtime` run on
io_uring measures the cost of the `Local` thread check.

## Plan

Follow-ups from [moq-dev/moq#6176](https://github.com/moq-dev/moq/pull/6176),
decided 2026-10-21 with the maintainer.

- **Per-group clones.** The lite publisher clones its session and group for
  each group it serves. Borrow or share them instead, and show the win on the
  session delivery benches.
- **Type parameter.** Lite and IETF internals stay generic over the
  transport only so tests can inject mocks. Mock behind the boxed transport
  instead, and drop the parameter.
- **`Local` check.** Run `just bench-runtime` on io_uring and report what the
  `Local` thread check costs; act on it only if it shows.

Dropped: renaming the over-long `session_delivery_*` bench IDs, since it
loses their history for a cosmetic gain.

Public API: none. Wire: none.
