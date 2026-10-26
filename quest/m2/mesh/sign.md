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
- A frame that fails verification ends its group with a typed error; nothing
  unverified reaches the application.
- Identity level carries no signature and trusts the token's path binding.
- Encryption nests on top later ([mesh keys](/quest/m3/mesh-keys.md)): keep
  room for it without building it.
- If the format is new wire bytes, specify it in a draft beside
  `draft-lcurley-moq-e2ee` with shared vectors; `just drafts check` passes.

Tests: a forged frame, a frame replayed from another group or track, and a
broadcast published under someone else's ID all refused; Rust/JS interop in
`just test interop`.
