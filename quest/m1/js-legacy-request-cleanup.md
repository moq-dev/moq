# [S] JS cleans up timed-out requests on drafts 14-16

## Goal

When a JS request on moq-transport drafts 14-16 times out after it reached
the peer, `js/net` sends the legacy cancellation before the local reset:
PUBLISH_NAMESPACE_DONE for an advertisement, UNSUBSCRIBE for a subscription.
Rust sends the same messages when it withdraws a namespace or cancels a
subscription. Unanswered advertisements stay terminal, as #5741 decided; only
the cleanup changes.

Today `Deadline` (`js/net/src/util/deadline.ts`) aborts every stream it
recorded when the deadline fires. On drafts 14-16 a request rides a virtual
control-stream stream whose reset never reaches the peer, so a timed-out
PUBLISH_NAMESPACE is never withdrawn and the peer can hold the namespace
until the session closes.

## Plan

Decided with the maintainer on 2026-10-17: the goal as drafted, from the
review of [#5741](https://github.com/moq-dev/moq/pull/5741), which put the fix
in `Deadline`'s timeout path rather than in each caller.

`#advertise` in `js/net/src/ietf/publisher.ts` loses its withdrawal this way.
The legacy SUBSCRIBE path opens its virtual stream outside `Deadline` and its
timeout cleanup already sends UNSUBSCRIBE (`#cancelSubscribe`); keep that
covered by a test while moving the mechanism. Check the other `Deadline`
users, such as SUBSCRIBE_NAMESPACE's UNSUBSCRIBE_NAMESPACE on drafts 14-15,
for the same gap. Draft 17 and later keep the stream reset alone.

Rust's own advertisement timeout (`read_response` in
`rs/moq-net/src/ietf/publisher.rs`) also drops the request without
PUBLISH_NAMESPACE_DONE on drafts 14-16. It is outside this quest as planned.

Test with a mocked clock on drafts 14, 15 and 16: a timed-out
PUBLISH_NAMESPACE whose message was sent is followed by PUBLISH_NAMESPACE_DONE
on the control stream. Wire: no new messages.
