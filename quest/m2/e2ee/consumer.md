# [M] One encrypted consumer shape for every player

## Goal

Encrypted playback has one consumer shape, the mirror of the protected
producer, that the browser and the CLI share, so each player does not
reinvent decryption and physical-name mapping.

## Plan

Decided 2026-10-20 (maintainer): the shape is the moq-net consumer traits.
`moq_e2ee::broadcast::Consumer` and its track and group consumers implement
them ([net-wrap](/quest/m2/net-wrap/README.md)), so a Rust player, the CLI,
and `@moq/watch` open an encrypted broadcast as they open any other,
because every reader already takes the traits. Kept in m2 (decided
2026-10-17).

What remains here:

- Resolve the opaque path, take the epoch from its route (or from the
  application on a wire without route epochs), and bind it with
  `credential.consume(epoch)` in one call each player shares, in Rust and JS
  with matching names.
- Keep that generation while the route keeps the same epoch, across
  re-announcements: each key's AEAD budget lives in the generation, so
  re-binding on every announce would let a relay that withdraws and
  re-announces reset it.
- Decrypt the protected catalog before the existing selection.
- Rust and JS only (decided 2026-10-20, maintainer): the e2ee publisher has
  no moq-ffi half either, so an FFI consumer waits for a quest that adds both.

Public API: an e2ee consumer entry point in `moq-e2ee` and `@moq/e2ee`.
Wire: none.

## Required

- [Dynamic names](/quest/m2/e2ee/dynamic-names.md) - the approved naming design can change the physical-name mapping this fixes
- [Wrapper traits over moq-net](/quest/m2/net-wrap/README.md) - Rust and JS readers take the traits the e2ee consumer implements
- [Rust protected publisher seams](/quest/m2/e2ee/rust-publish.md) - the producer this mirrors
