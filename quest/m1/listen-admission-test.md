# [XS] Test that moq --listen bounds admission

## Goal

An in-process test proves that `moq --listen` bounds auth admission by
`listen.timeout`: an admission that never answers closes at the deadline, one
answered just inside it is admitted, and `"0"` waits. Today
`rs/moq-cli/tests/listen.rs` drives the binary with `--auth-public`, where
admission never waits, so nothing covers `serve_client`
(`rs/moq-cli/src/main.rs`) running admission under the handshake deadline.

## Plan

Decided with the maintainer on 2026-10-20, from the follow-ups of
[#5765](https://github.com/moq-dev/moq/pull/5765), which implements the
deadline.

- Drive `serve_client` in process on controlled time, with an auth decider
  that can stall, rather than the binary on wall time. If it cannot be driven
  that way, open the smallest seam that lets it.
- Match the relay's `auth_lifetime` cases so the two listeners stay alike.

Public API: none. Wire: none.

## Required

- [Handshake deadline covers admission](/quest/m1/handshake-admission-deadline.md) - the bound this tests
