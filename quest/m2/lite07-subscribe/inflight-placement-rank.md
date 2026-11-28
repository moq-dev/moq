# [M] Re-rank in-flight groups when a relay places a provisional live join

## Goal

A lite-07 group stream already open re-ranks when the ranges covering its group change because a range was placed, not only when a SUBSCRIBE_UPDATE edits them. On a relay, a live join waiting for its upstream's answer covers every group up to its end; once upstream places it at a newer group, a history stream opened meanwhile drops to the history range's priority and order at once, rather than keeping the join's urgent rank until it finishes, which backpressure can stretch. Rust and JS match.

## Plan

- Rust (`rs/moq-net/src/lite/publisher.rs`): `TrackRun::update` refreshes `Steer::inflight` (the Range IDs covering each group in flight, including the waiting and deferred groups) only after an edit. Recompute it when the subscriber's window places a range too. Learn of the placement through the track subscriber, as a placement revision the run loop polls with its other track events (`AGENTS.md`: no polling endpoint or timer loop), then write the steer only if a group's covering IDs changed.
- JS (`js/net/src/lite/publisher.ts`): `rankingOf` re-reads `servingCovering` on each rank; have the ranking listen to the subscription's `startChanged` so a placement re-ranks open streams.
- Test in both languages, failing first: a relay copy with a provisional live join and a history range, a history group's stream open and backpressured, then upstream places the join past it; the stream's rank falls to the history range's at once.
- Check `/verify-subscribe`'s `EffectivePolicy` coverage row and bump its sync commit.

Public API: none. Wire: none.

## Related

- [Finalize lite-07](/quest/m2/lite07-finalize.md) - the cut this scheduling fix lands before
