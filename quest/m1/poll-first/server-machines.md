# [S] Server::accept and Request::ok are kio machines

## Goal

moq-net's `Server::accept` and `server::Request::ok` are `#[kio::machine]`
`poll_*` methods with one-line async wrappers, like the other straight-line
bodies converted in [moq-dev/moq#7284](https://github.com/moq-dev/moq/pull/7284). Behavior, errors, and their public async
signatures stay the same.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7284](https://github.com/moq-dev/moq/pull/7284).

Milestone: ✅ m1. Not chosen: m2.

- Both are straight-line in spirit (`rs/moq-net/src/server.rs:283` and
  `:768`), but they wait on the session handshake, which has no poll form, so
  [moq-dev/moq#7284](https://github.com/moq-dev/moq/pull/7284) left them as `async fn`.
  [Generated async wrappers](/quest/m1/poll-first/async-feature.md) converts
  the handshake by hand; once it has, convert these two through
  `#[kio::machine]`, following the conventions in `kio::machine`'s docs and
  [straight-line bodies become machines](/quest/m1/poll-first/machine-bodies.md).
- If the handshake's poll form makes either body a loop rather than
  straight-line, hand-write that `poll_*` instead and say so in the PR.

Public API: adds `poll_accept`/`poll_ok` beside the existing async methods.
Wire: none.

## Required

- [Generated async wrappers](/quest/m1/poll-first/async-feature.md) - gives the handshake a poll form these bodies wait on
