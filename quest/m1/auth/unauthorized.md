# [S] A subscription that loses access resets with UNAUTHORIZED

## Goal

When a grant shrinks and a subscription, fetch, or announce loses access,
its stream resets with a dedicated `UNAUTHORIZED` stream code, so the peer
tells revocation apart from a session closing. Today it resets with
`0x3 SESSION_CLOSED` (`StreamError::Session(Unauthorized)`).

## Plan

Assign `0x3A UNAUTHORIZED` in the lite draft's stream error table, the next
code in the application block after `TIMESTAMP_MISMATCH`, on lite-06: a
published lite-06 peer already maps an unknown stream code to a generic
error, so the addition is compatible. Add the matching `StreamError`
variant (the enum is non-exhaustive) in Rust and JS, send it from every
revocation path the lite stream added, and cover it in the Rust and JS
tests. Map it to the existing `Unauthorized` protocol kind in
`stream_kind` in `rs/moq-ffi/src/error.rs` and `rs/libmoq/src/error.rs`,
whose wildcard arms would otherwise report `Unknown`, with a test for each.
Update `drafts/draft-lcurley-moq-lite.md` and run `just drafts check`.
