# [S] Wide varints are tested where the two forms differ

## Goal

The plain-`u64` varint codec (#4437) has tests for the cases its first round
left open:

- The wide-sequence relay test asserts the error a lite-06 reader sees for a
  group of `1 << 62` from a lite-07 publisher, not any error. Today the reader
  only sees `Stream(Internal)` over the wire, so decide what it should see.
- A subscription whose groups turn wide partway through fails only that
  subscription, and the session and other tracks keep going.
- Announce compression is tested where the QUIC and leading-ones forms size
  the same value differently.

## Plan

Extend `rs/moq-net/tests/wide_sequence.rs` and the lite announce encode tests.

Public API: none. Wire: none.
