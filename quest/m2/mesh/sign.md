# [M] Signed broadcasts

## Goal

A node's persistent Ed25519 endpoint key signs every frame it publishes at the
authentication level, and subscribers deliver only frames that verify against
the endpoint ID in the broadcast path, so a relay can proxy without being able
to forge or splice content. Each broadcast picks its level: identity or
authentication, with authentication the mesh default. Rust and JS interoperate.

## Plan

Decided 2026-10-25 (maintainer): per-frame signatures, not hash chains or
per-group checkpoints; zero verification delay is worth 64 B per frame. The
endpoint ID is the public key in an iroh-style text encoding, persisted with
`0600` permissions and never rotated implicitly.

- Bind each signature to the broadcast path, track, group, and frame index, so
  a relay cannot replay, reorder, or move a frame between tracks.
- At the authentication and encryption levels a frame reaches the
  application only after it verifies; a frame that fails ends its group with a
  typed error, which a [byte stream](/quest/m2/stream/README.md) surfaces as a
  reset. Only the identity level flushes partial frames as they arrive
  (decided 2026-10-25, maintainer). A stream's frame is one socket read, so the
  sender's frame-size cap bounds the added latency.
- Identity level carries no signature and trusts the token's path binding.
- Encryption nests on top later ([mesh keys](/quest/m3/mesh-keys.md)): keep
  room for it without building it.
- If the format is new wire bytes, specify it in a draft beside
  `draft-lcurley-moq-e2ee` with shared vectors; `just drafts check` passes.

Tests: a forged frame delivering zero bytes to the application, a forged frame, a frame replayed from another group or track, and a
broadcast published under someone else's ID all refused; Rust/JS interop in
`just test interop`.
