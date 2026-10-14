# [S] JS refuses moq-transport input it cannot carry

## Goal

`js/net`'s IETF subscriber ends a track with an unsupported error, logged
at warn with STOP_SENDING on the stream, when a publisher sends a subgroup
other than 0 or a datagram past object 0, as Rust does since
[#5486](https://github.com/moq-dev/moq/pull/5486). It no longer drops them
quietly and serves a truncated group.

## Plan

Decided 2026-10-13 (maintainer): JS parity is its own quest; #5486 merged
the Rust half with a Rust-only note in `doc/concept/standard.md`. JS already
answers malformed groups with STOP_SENDING.

- Mirror Rust's refusal in `js/net/src/ietf/`, with the same error and
  stop code, and keep the session open.
- Drop the Rust-only note from `doc/concept/standard.md`'s deviations.
- Regressions per case, each failing without the fix, plus a byte-level
  interop case if the Rust/Bun harness covers ingest.

Public API: none. Wire: none (refusal behavior only).
