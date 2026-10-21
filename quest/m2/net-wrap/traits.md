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
  `group::Producer`, `group::Consumer`, `frame::Producer`, and
  `frame::Consumer`. Associated types link the levels, so a broadcast
  producer's `create_track` returns its own track producer, whose
  `create_group` returns its own group producer, whose `create_frame`
  returns its own frame producer. moq-mux's fMP4 and LOC importers stream
  chunks through `create_frame` and `write`, and readers use `next_frame`
  and `read_chunk`, so without the frame level they stay concrete or buffer
  whole frames. `frame::Producer<'_>` borrows its group, so its associated
  type is generic over that lifetime.
- `poll_*` methods are the required ones; the async forms are provided
  methods over them, so no `async fn` in a trait forces a `Send` bound.
- Demand (`demand`, `subscription_changed`, `Request::accept` and `reject`)
  and terminal state (`finish`, `abort(self, Error)`, `closed`) are trait
  methods, so a wrapper forwards them rather than reimplementing them.
- Inline moq-net's impls so a monomorphized caller compiles to today's code.
  Prove it on `rs/moq-net/benches` (`group.rs`, `frame.rs`) before and after:
  no regression on the plaintext path.
- A pass-through test wrapper implements every trait and checks that
  finish, a typed abort, demand, and preference updates reach the other
  side at each level. It sits behind a hidden, off-by-default moq-net
  feature, like `fuzz`, so moq-json, moq-stats, and hang reuse it from
  their dev-dependencies.
- Decide whether the existing `Consume<T>` in `model/origin.rs` folds into
  these traits or stays separate.
- Update `doc/concept` where it names the concrete types, and add the traits
  to `MIGRATE.md`.

Public API: new moq-net traits, plus the hidden test-wrapper feature. Wire: none.
