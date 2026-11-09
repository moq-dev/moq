# [L] moq-net phase data

## Goal

moq-net's session, subscription, and auth state machines, in Rust and in
js/net, keep each phase's data inside that phase's variant. No `.expect()`
or call ordering stands in for a phase. A placeholder such as
`RequestId(0)` never stands in for "not assigned yet".

## Plan

The shared decisions are in the [epic](/quest/m1/explicit-state/README.md).

No reachable bug is folded into what remains. Check each item below while
redesigning it, and reproduce any inconsistent state you find.

[moq-dev/moq#8275](https://github.com/moq-dev/moq/pull/8275) landed the IETF
publisher's `TrackServe` phase and its reopened-tail fix, the datagram
forward, the session drivers' terminal result, IETF `Dispatch`, lite
`PeerSetup`, the listing `Queue`, and the js/net connections' close phase.
[moq-dev/moq#8322](https://github.com/moq-dev/moq/pull/8322) landed js/net's
`Join`, `RequestStream.refuse`, `Lister`, auth counters and `IssuedGrant`,
`Reload.status`, the signals `Effect` stale read, and Rust's `Following`.

The lite items overlap
[moq-dev/moq#8220](https://github.com/moq-dev/moq/pull/8220) (lite publisher
and subscriber, `fills.rs`, js/net lite). Start each once it has landed or
closed.

Rust:

- **IETF `SubscribeOnce`/`Established`** (`rs/moq-net/src/ietf/subscriber.rs`):
  - `OnceStep` exists, but `stream`, `messages`, `fill`, `established`,
    `held`, `handoff`, and `target` sit outside it. About 20
    `.expect("established")` calls stand in for the step.
  - `failed`, `ended`, `finished`, `fetch_done`, and `OnceStep::Settle`
    duplicate one another.
  - `request_id: RequestId(0)` is a placeholder for an id that isn't
    assigned yet.
- **Lite subscriber** (`rs/moq-net/src/lite/subscriber.rs`):
  - `ServeLoop.finishing` and `parked` belong inside `SubStream`.
  - `SubStream.served` plus `started` are really `Option<Option<u64>>`.
- **`auth::Slot`** (`rs/moq-net/src/auth.rs`): `streamed`, `refused`,
  `withdrawn`, `grant`, `answered`, `retry`, and `ended` are seven fields
  coordinated by hand. Its tests call `granted` on slots whose stream never
  opened, so the phase enum has them open the stream first.
- **Smaller ones**:
  - `model/origin.rs` `TrackIo` keeps `request` and `accepted` side by side.
  - `fills.rs` `Recovery`.

JS (`js/net`):

- **Lite `SubscribeEntry`** (`lite/subscriber.ts`): eleven optional fields.
- **Origin `Request` and `OriginState`** (`origin.ts`): the closed state is
  encoded by about 40 `?.` guards.

Keep the lite `TrackRun` start state out of this child. It belongs to
[Delete the old start bookkeeping](/quest/m2/subscribe-ranges/start-bookkeeping.md).

## Related

- [Delete the old start bookkeeping](/quest/m2/subscribe-ranges/start-bookkeeping.md) - owns lite `TrackRun`'s start state
