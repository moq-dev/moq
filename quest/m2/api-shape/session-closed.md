# [S] A peer's closed session reads as Closed in the bindings

## Goal

A binding sees `MoqError::Closed` when the session behind a track or group
closed, whether it closed locally or on a peer across a real session. Today
only a local close maps to `Closed` (`rs/moq-ffi/src/error.rs`, the
`moq_net::Error` conversion). A peer's SESSION_CLOSED reaches the bindings as
`Protocol { kind: SessionClosed }` on moq-lite, or `Internal` on
moq-transport. A test across a relay checks the error a binding sees.

## Plan

Planned 2026-11-09 (maintainer, /quest-complete follow-up of
[moq-dev/moq#8373](https://github.com/moq-dev/moq/pull/8373), which guarantees
`Closed` only for two endpoints sharing one in-process origin). ✅ m2, in this
epic, since it changes the bindings' public errors (rejected: m1 as additive;
dropping it and documenting the difference).

Map the received SESSION_CLOSED to `Closed` where moq-ffi converts errors. If
moq-transport decodes it as `Internal` before moq-ffi sees it, fix that
decoding in moq-net rather than guessing in the binding. Check whether
`js/net` exposes the same split and mirror the result there, per the
cross-package rule. Its Impact section says how binding callers that match
`SessionClosed` migrate.

Public API: breaking for the bindings' error kinds. Wire: none.
