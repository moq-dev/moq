# [M] moq-net phase data

## Goal

moq-net's session, subscription, and auth state machines, in Rust and in
js/net, keep each phase's data inside that phase's variant. No `.expect()`
or call ordering stands in for a phase. A placeholder never stands in for
"not assigned yet".

## Plan

The shared decisions are in the [epic](/quest/m1/explicit-state/README.md).

No reachable bug is folded into what remains. Check each item below while
redesigning it, and reproduce any inconsistent state you find.

[moq-dev/moq#8275](https://github.com/moq-dev/moq/pull/8275),
[moq-dev/moq#8322](https://github.com/moq-dev/moq/pull/8322), and
[moq-dev/moq#8402](https://github.com/moq-dev/moq/pull/8402) landed the rest:
IETF `SubscribeOnce`, `auth::Slot`, `TrackIo`, `Recovery`, the lite
`SubStream`, js/net's origin, and the lite `SubscribeEntry`'s start.

Rust (`rs/moq-net/src/ietf/subscriber.rs`): `JoinFetch`, `LearnTrackStatus`,
and `FetchRun` keep `stream: Option<Stream>` outside their step enums, so
about ten `.expect("the request stream is open")` calls stand in for the step.
Carry the stream in the steps that hold it, as `SubscribeOnce` does.

JS (`js/net/src/lite/subscriber.ts`): `SubscribeEntry` still keeps phases as
optional fields: `held` and `answered` until the first response, and `fin`,
`finished`, and `responses` for how its stream ends.

Keep the lite `TrackRun` start state out of this child. It belongs to
[Delete the old start bookkeeping](/quest/m2/subscribe-ranges/start-bookkeeping.md).

## Related

- [Delete the old start bookkeeping](/quest/m2/subscribe-ranges/start-bookkeeping.md) - owns lite `TrackRun`'s start state
