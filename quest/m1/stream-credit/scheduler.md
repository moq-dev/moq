# [L] One priority entry point for streams

## Goal

In Rust (`rs/moq-net`) and JS (`js/net`), every stream a session opens
(request streams, group streams, sockets) goes through one entry point that
hands out the peer's stream credit by priority. Without credit, an open waits
its turn instead of failing with `NoStreamCredit`, and the relay waits for a
publisher's credit instead of refusing the subscription. moq-quic and
`@moq/qmux` expose their remaining credit and blocked state to it.

A subscription that does give up ends its reader with the cause: a JS read
rejects instead of hanging, and the refusal is told apart from a bare
`Internal` (code 0). Folded in 2026-11-04 (maintainer) rather than a separate
quest, since the scheduler removes the refusal that caused the hang.

## Plan

See the [epic](/quest/m1/stream-credit/README.md) for the decisions. Opens
in `js/net` happen today in `stream.ts`, `ietf/request_stream.ts`,
`ietf/subscriber.ts`, and `ietf/auth.ts`, and in Rust's lite and ietf
sessions; route them all through the entry point. Priority follows the
existing track and request priorities, the same ranking
[shared group serving](/quest/m2/shared-serve.md) uses for group streams;
keep one ranking, not two. First confirm the JS reader's hang on a refused
subscription is not intentional, for example a wait for a re-route.

Tests: in both languages, more opens than the peer's credit all complete in
priority order as credit returns; a relay whose publisher runs out of credit
serves the later subscriptions once credit returns; a JS read on a
subscription that gives up rejects with its cause.

Public API: `NoStreamCredit` is no longer raised for an open that can wait.
Wire: none.
