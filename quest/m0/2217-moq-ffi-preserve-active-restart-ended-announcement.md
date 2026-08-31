# [S] moq-ffi: preserve Active/Restart/Ended announcement lifecycle (AI review)

## Goal

Implement and verify the behavior tracked in [#2217](https://github.com/moq-dev/moq/issues/2217)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

#### Problem

`MoqAnnounced.next()` currently skips `Ended` events and converts both `Active` and `Restart` into `MoqAnnouncement`. All generated language wrappers therefore lose announcement lifecycle information.

Its documentation suggests waiting for `broadcast.closed()` to learn that a broadcast was unannounced, but Rust intentionally keeps the announcement guard independent from the broadcast producer. A publisher can unannounce a still-live broadcast, so broadcast closure is not an equivalent signal.

The FFI publisher API also retains the announcement guard internally until the broadcast closes, which prevents explicit unannounce while the broadcast remains usable. libmoq already exposes an origin publication handle and explicit unpublish behavior.

#### Suggested direction

- Return a typed UniFFI announcement event that preserves `Active`, `Restart`, and `Ended`.
- Expose an owned announcement/publication handle, or an explicit unannounce operation, whose lifetime is independent from the broadcast.
- Update Python, Swift, Kotlin, and Go wrappers and examples together.
- Add tests for unannounce-without-close and restart delivery.

## Closes

- [#2217](https://github.com/moq-dev/moq/issues/2217) - close this issue when the quest finishes
