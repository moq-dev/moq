# [S] A scoped cursor reports a lost route as offline when a broader route takes over

## Goal

A reader on a scoped announcement cursor whose exact route is lost with its
session, while a broader route still covers the same presented path, sees the
lost route go offline, never an END. An END still means the publisher
withdrew.

## Plan

Planned 2026-11-10 (maintainer, /quest-plan on lite-07 routing). Milestone:
✅ m1 (rejected: m2 in the routes epic; m0).

Found by the automated review of
[moq-dev/moq#8375](https://github.com/moq-dev/moq/pull/8375), posted fifteen
minutes before it merged and never addressed. With a covering cursor rooted
at `room/alice`, an exact forwarded route presents with captures `Some([])`
and the broader `room` fallback with `None`. Losing the exact route's session
passes `Gone::Offline` into the update, but the capture-change branch
(`rs/moq-net/src/model/origin.rs`, about line 1753, the arm that replaces a
presented route whose captures differ) retracts the old identity with
`Gone::End`. Readers then see End, as if the publisher had withdrawn.

Reproduce it first by extending
`capture_change_retracts_before_reannouncing_a_presented_prefix` with a
forwarded route lost with its session. Then carry the actual removal reason
through that replacement instead of a fixed `Gone::End`.

Check the bindings that surface the reason (`moq-ffi`, and the Swift, Kotlin,
Python, and C wrappers of `announced`) need no change beyond the Rust fix.

Caused by: [moq-dev/moq#8375](https://github.com/moq-dev/moq/pull/8375), which
introduced the offline reason and left this branch on End.

Public API: none. Wire: none.
