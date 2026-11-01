# [L] moq-net producer and consumer traits

## Goal

moq-net exports traits for the broadcast, track, group, and frame producers
and consumers, datagrams included, and its own types implement them with no
change in behavior or cost. A crate generic over the traits compiles against
moq-net's types by default and against a wrapper's types when handed one.

## Plan

See the [questline](/quest/m2/net-wrap/README.md) for the decisions: generics
with moq-net's types as the default, no `Send` bounds beyond moq-net's own, and
the methods exactly as moq-net has them today.

- Every handle a writer or reader can get from moq-net's public API,
  including pending operations (`track::Subscribing`, `Resolving`,
  `Fetching`), requests, and adapters, has an associated type, so no trait
  method returns a concrete moq-net type and a wrapper never leaks its
  physical handle. Examples of what the rule covers:
  - The producer chain: a broadcast producer's `create_track` returns its
    own track producer, whose `create_group` returns its own group
    producer, whose `create_frame` returns its own frame producer.
    moq-mux's fMP4 and LOC importers stream chunks through `create_frame`
    and `write`, and readers use `next_frame` and `read_chunk`, so without
    the frame level they stay concrete or buffer whole frames.
    `frame::Producer<'_>` borrows its group, so its associated type is
    generic over that lifetime.
  - Requests: a broadcast producer's `requests` returns its own
    `broadcast::Requests`, whose `requested_track` yields its own
    `track::Request`, so generic code serves undeclared names (as
    [E2EE undeclared names](/quest/m2/e2ee/undeclared-names.md) needs). A
    `track::Requests`'s `requested_fill` yields its own `track::Fill`, whose
    `accept` returns its own group producer and whose `drop_groups` names
    the rest dropped.
  - Pending operations: `track::Consumer::subscribe` resolves to the
    wrapper's own subscriber, so E2EE decrypts behind it; `Resolving` and
    `Fetching` resolve to the wrapper's logical metadata and groups.
  - Adapters: a subscriber's `ordered` returns its own `track::Ordered`
    with the same group, datagram, and control surface. moq-json's snapshot
    and window consumers, moq-mux, and moq-stats read through `ordered()`,
    so without it they stay concrete.
  - Listing (maintainer, 2026-10-24): a broadcast consumer's `tracks`
    returns its own `Tracks` reader, so a wrapper reports the tracks it
    lists under its own names, and moq-archive's listing reader can take
    the trait.
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
- Update `doc/concept` where it names the concrete types, and say in the
  Impact bullet how to migrate to the traits.

Public API: new moq-net traits, plus the hidden test-wrapper feature. Wire: none.

Decided 2026-10-22 (maintainer): the traits break moq-net's public API, so
they wait for the breaking release cut.

## Required

- [Breaking release cut](/quest/m2/release/cut.md) - the traits land after the cut that ships m1's breaks
