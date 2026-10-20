# [L] moq-net producer and consumer traits

## Goal

moq-net exports traits for the broadcast, track, group, and frame producers
and consumers, datagrams included, and its own types implement them with no
change in behavior or cost. A crate generic over the traits compiles against
moq-net's types by default and against a wrapper's types when handed one.

## Plan

See the [questline](/quest/m2/net-wrap/README.md) for the decisions: generics
with moq-net's types as the default, no `Send` bounds wasm can't meet, and
the methods exactly as moq-net has them today.

- Cover each role writers and readers hold: `broadcast::Producer`,
  `broadcast::Consumer`, `track::Producer`, `track::Request`,
  `track::Dynamic`, `track::Consumer`, `track::Subscriber`,
  `group::Producer`, and `group::Consumer`. Associated types link the
  levels, so a broadcast producer's `create_track` returns its own track
  producer, whose `create_group` returns its own group producer.
- `poll_*` methods are the required ones; the async forms are provided
  methods over them, so no `async fn` in a trait forces a `Send` bound.
- Demand (`demand`, `subscription_changed`, `Request::accept` and `reject`)
  and terminal state (`finish`, `abort(self, Error)`, `closed`) are trait
  methods, so a wrapper forwards them rather than reimplementing them.
- Inline moq-net's impls so a monomorphized caller compiles to today's code.
  Prove it on `rs/moq-net/benches` (`group.rs`, `frame.rs`) before and after:
  no regression on the plaintext path.
- A pass-through test wrapper in moq-net's tests implements every trait and
  checks that finish, a typed abort, demand, and preference updates reach
  the other side at each level.
- Decide whether the existing `Consume<T>` in `model/origin.rs` folds into
  these traits or stays separate.
- Update `doc/concept` where it names the concrete types, and add the traits
  to `MIGRATE.md`.

Public API: new moq-net traits. Wire: none.
