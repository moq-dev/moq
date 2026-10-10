# [M] Rust E2EE separates producing and consuming generations

## Goal

The Rust E2EE core produces only under an epoch it minted itself. A discovered
or shared epoch binds a consume-only generation, whose type cannot produce.
Rust publisher integration and the TypeScript core build on the same settled
surface without depending on each other's media integration.

## Plan

Decided in the 2026-10-10 audit: split the core change out of Rust protected
publisher seams, since TypeScript needs its API but not its Hang integration.

`Credential::generation(epoch)` in `rs/moq-e2ee/src/credential.rs` currently
lets a caller produce under any epoch. Replace it with a minting call that
takes no epoch and returns a producing generation, plus a consume-only
binding for a discovered epoch. Preserve the questline's generation methods
and shared publisher claims so clones cannot reopen a track with reset nonce
counters. Settle the public names during this quest; TypeScript mirrors them.
The fixed-epoch constructor for known-answer vectors stays crate-private.

Keep the core independent of Hang and codec-specific publication. Cover the
producing/consuming boundary, fresh epochs, shared claims, and existing
vectors in the crate's CI tests. Update the E2EE draft's epoch rule and the
existing Rust E2EE documentation inline; no separate guide is needed for
this split. Run the crate checks, `just drafts check`, and the cross-language
interop suite required for wire work if implementing the change touches the
wire.

Public API: breaking generation construction and separate producing and
consuming types in moq-e2ee. Wire: unchanged; the existing epoch derivation
and ciphertext format stay the same.
