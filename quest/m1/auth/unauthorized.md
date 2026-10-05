# [S] A subscription that loses access resets with UNAUTHORIZED

## Goal

When a grant shrinks and a subscription, fetch, or announce loses access,
its stream resets with a dedicated `UNAUTHORIZED` stream code, so the peer
tells revocation apart from a session closing. Today it resets with
`0x3 SESSION_CLOSED` (`StreamError::Session(Unauthorized)`).

## Plan

Assign `0x3A UNAUTHORIZED` in the lite draft's stream error table, the next
code in the application block after `TIMESTAMP_MISMATCH`, in the wip lite
version that carries the AUTH stream (`moq-lite-07-wip` today), never a
published one in place (decided 2026-10-05). Add the matching `StreamError`
variant (the enum is non-exhaustive) in Rust and JS, send it from every
revocation path the lite stream added, and cover it in the Rust and JS
tests. Map it to the existing `Unauthorized` protocol kind in
`stream_kind` in `rs/moq-ffi/src/error.rs` and `rs/moq-c/src/error.rs`,
whose wildcard arms would otherwise report `Unknown`, with a test for each.
Update `drafts/draft-lcurley-moq-lite.md` and run `just drafts check`.

## Required

- [Lite stream](/quest/m1/auth/lite.md) - the revocation paths that send it
