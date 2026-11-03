# [S] The E2EE nonce binds the group start

## Goal

The E2EE nonce binds a group's start timestamp instead of its sequence
number, in the draft and every implementation, so encrypted groups survive
the cut.

## Plan

Update `drafts/draft-lcurley-moq-e2ee.md`, `rs/moq-e2ee`, and the JS side,
which has landed: `js/e2ee/src/derive.ts` builds `nonce(group, frame)` (about
line 156). The start is unique per track because starts strictly increase.
Run `just drafts check`.

Public API: none. Wire: the E2EE nonce input changes.

## Required

- [Rust groups by start](/quest/m1/time-addressing/model.md) - the start the nonce binds
- [@moq/net by start](/quest/m1/time-addressing/js.md) - the JS start the JS half binds
