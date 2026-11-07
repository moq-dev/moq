# [S] Transports report no stream credit

## Goal

`moq_net::transport::StreamCredit`, `Dir`, and the `poll::Session::stream_credit`
hook are gone, so no transport implements a credit API. The open line
(`rs/moq-net/src/transport/open.rs`) decides when a waiting open preempts a
lower-ranked stream from its own state, as `js/net` already does, and an
open waiting on something other than credit still never triggers a reset.

## Plan

Decided 2026-11-07 (maintainer, /quest-plan from the API shape audit):
✅ "Delete, no hooks" (rejected: keeping the hook and recording why). This
follows the [moq-dev/moq#7894](https://github.com/moq-dev/moq/pull/7894)
reversal recorded in this epic ("the signal is the open line's waiting
state, with no transport credit or blocked API") and overrides
[moq-dev/moq#8088](https://github.com/moq-dev/moq/pull/8088)'s refutation,
which kept the hook because the open line reads it.

The hook exists today so an open waiting on a WebTransport stream header is
not mistaken for a credit wait (`make_room`, open.rs). Remove that case at
its source, for example by having a transport's open complete only once
credit is granted and write the header after, so a pending head always means
credit. Implementers of `poll::Session` (moq-tokio, moq-uring, qmux, the
memory transport) drop their implementations.

Breaking for transport implementers; the Impact section says what replaces
the hook.
