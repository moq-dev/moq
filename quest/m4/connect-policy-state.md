# [M] The connect policy's state lives in one place, in both languages

## Goal

The client connect policy's lifecycle is stored once per language:
`moq_tokio::Connection` reads a snapshot of `moq_net::Reconnect`'s state
rather than rebuilding it from the action stream, and `js/net`'s `Reload`
runs a port of the same policy object, with the same inputs and actions, so a
Rust fix is a one-to-one diff in JS and one fixture table checks both.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
The lifecycle is kept three times: `Reconnect` (`rs/moq-net/src/reconnect.rs`),
moq-tokio's `Lifecycle::Live { migrating, predecessor }`
(`rs/moq-tokio/src/connection.rs`), and about ten private fields in
`js/net/src/connection/reload.ts`, which no model checks. About 22 reconnect
and GOAWAY fixes landed since the last release, and
[moq-dev/moq#8595](https://github.com/moq-dev/moq/pull/8595),
[moq-dev/moq#8597](https://github.com/moq-dev/moq/pull/8597), and
[moq-dev/moq#8737](https://github.com/moq-dev/moq/pull/8737) each had to be
hand-ported to `reload.ts`; REVIEW.md reads a line of fixes like that as a
design problem. moq-tokio's `Retired` then `Offline` double write briefly
publishes an aborted session.

It makes JS the same shape the
[trace validation](/quest/m4/reconnect-trace-validation.md) checks, so that
quest can cover both languages.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (a) port the policy object to JS and snapshot it in moq-tokio (rejected: (b) wait for rs2ts; (c) JS traces only).

Public API: none. Wire: none.

