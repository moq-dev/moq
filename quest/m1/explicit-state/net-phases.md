# [L] moq-net phase data

## Goal

moq-net's session, subscription, and auth state machines, in Rust and in
js/net, keep each phase's data inside that phase's variant. No `.expect()`
or call ordering stands in for a phase. A placeholder such as
`RequestId(0)` never stands in for "not assigned yet".

## Plan

The shared decisions are in the [epic](/quest/m1/explicit-state/README.md).

No reachable bug is folded into this child. Check each item below while
redesigning it, and reproduce any inconsistent state you find.

Rust:

- **IETF `SubscribeOnce`/`Established`** (`rs/moq-net/src/ietf/subscriber.rs`):
  - `OnceStep` exists, but `stream`, `messages`, `fill`, `established`,
    `held`, `handoff`, and `target` sit outside it. About 20
    `.expect("established")` calls stand in for the step.
  - `failed`, `ended`, `finished`, `fetch_done`, and `OnceStep::Settle`
    duplicate one another.
  - `request_id: RequestId(0)` is a placeholder for an id that isn't
    assigned yet.
- **IETF publisher** (`rs/moq-net/src/ietf/publisher.rs`):
  - `RunSubscription.served` caches the track's drained state.
  - An update that reopens `TrackServe` (`draining = false`, a new `tail`)
    is never polled again. Reproduce this first; it is the audit's plausible
    finding.
  - `TrackServe.draining`/`end` and `DatagramForward` should become explicit
    phases.
- **Lite subscriber** (`rs/moq-net/src/lite/subscriber.rs`):
  - `ServeLoop.finishing` and `parked` belong inside `SubStream`.
  - `SubStream.served` plus `started` are really `Option<Option<u64>>`.
- **`auth::Slot`** (`rs/moq-net/src/auth.rs`): `streamed`, `refused`,
  `withdrawn`, `grant`, `answered`, `retry`, and `ended` are seven fields
  coordinated by hand.
- **`Following`** (`rs/moq-net/src/model/follow.rs`): eight fields, of which
  `last`, `moved`, and `absorbed` are always reset together.
- **Smaller ones**:
  - `ietf/session.rs` `Dispatch` holds two `Option`s, exactly one of which is
    set.
  - `model/origin.rs` `TrackIo` keeps `request` and `accepted` side by side.
  - The driver `ending` field.
  - The lite `PeerSetupState`.
  - The listing `Queue`.
  - `fills.rs` `Recovery`.

JS (`js/net`):

- **Auth** (`auth_session.ts`):
  - `AuthSession`/`Presented` derive counters by hand at six sites.
  - `IssuedGrant` sets `#done` asynchronously, so two updates in one tick
    both pass its check.
- **`Reload`** (`connection/reload.ts`): `status` is written at seven sites,
  each re-deriving it from `established`, `#draining`, and `#dialing`.
- **Lite `SubscribeEntry`** (`lite/subscriber.ts`): eleven optional fields.
- **`Lister`** (`listing.ts`): it still writes into the listing after
  `close()`.
- **`RequestStream`** (`ietf/request_stream.ts`): `refuse()` leaves the stream
  open if writing the refusal throws.
- **Signals `Effect`** (`js/signals/src/index.ts`): `get()` during the stale
  window pushes into the next run's unwatch list.
- **Origin `Request` and `OriginState`** (`origin.ts`): the closed state is
  encoded by about 40 `?.` guards.
- **`Join`** (`ietf/join.ts`): a second fetch stream after the track ended
  leaks the first producer.
- **Connections** (`lite/connection.ts`, `ietf/connection.ts`): lite's
  `abort()` has no guard.

Keep the lite `TrackRun` start state out of this child. It belongs to
[Delete the old start bookkeeping](/quest/m2/subscribe-ranges/start-bookkeeping.md).

## Related

- [Delete the old start bookkeeping](/quest/m2/subscribe-ranges/start-bookkeeping.md) - owns lite `TrackRun`'s start state
