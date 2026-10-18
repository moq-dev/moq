# [S] Timed-out requests clean up on drafts 14-16

## Goal

When a request on moq-transport drafts 14-16 times out after it reached the
peer, Rust and JS both send the legacy cleanup message before the local
reset: PUBLISH_NAMESPACE_DONE for an advertisement, and UNSUBSCRIBE_NAMESPACE
for a namespace subscription on drafts 14-15. Unanswered advertisements stay
terminal, as #5741 decided; only the cleanup changes.

On drafts 14-16 a request rides a virtual control-stream stream whose reset
never reaches the peer, so today a timed-out PUBLISH_NAMESPACE is never
withdrawn and the peer can hold the namespace until the session closes:

- JS: `Deadline` (`js/net/src/util/deadline.ts`) aborts every stream it
  recorded when the deadline fires, before the caller can write the cleanup.
- Rust: when `read_response` in `rs/moq-net/src/ietf/publisher.rs` gives up
  on an advertisement, the request is dropped without PUBLISH_NAMESPACE_DONE.

## Plan

Decided with the maintainer on 2026-10-17:

- The goal comes from the review of
  [#5741](https://github.com/moq-dev/moq/pull/5741), which put the JS fix in
  `Deadline`'s timeout path rather than in each caller.
- Widened to Rust and JS in one quest, rather than a separate Rust quest,
  since both have the same gap on the same drafts.

The JS legacy SUBSCRIBE path opens its virtual stream outside `Deadline`, and
its timeout cleanup already sends UNSUBSCRIBE (`#cancelSubscribe`); keep that
covered by a test while moving the mechanism. Check every other timeout on
these drafts, in both languages, for the same gap. Draft 17 and later keep
the stream reset alone.

Test with mocked time on drafts 14, 15 and 16, in Rust and JS: a timed-out
PUBLISH_NAMESPACE whose message was sent is followed by PUBLISH_NAMESPACE_DONE
on the control stream, and the same for UNSUBSCRIBE_NAMESPACE on drafts
14-15. Wire: no new messages.
