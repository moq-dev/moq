# [S] moq-net's remaining surface fixes

## Goal

The rest of moq-net's small surface fixes, with their JS mirrors:

- After [moq-dev/moq#8063](https://github.com/moq-dev/moq/pull/8063) removes
  `RouteEvent::Takeover`, un-hide `origin::Consumer::routes`, `Routes`, and
  `RouteEvent`, documented as the relay-level stream that moq-relay and
  downstream relays already consume, and fix `request`'s doc to say what #8063 made it
  do. `RouteEvent` becomes `pub struct RouteEvent { pub kind: RouteKind, pub
  announce: Announce }` with `pub enum RouteKind { Start, Update, Restart, End }`,
  mirroring JS's `{ kind } & Routed`, so callers need no or-pattern and no
  accessors.
- Settle whether `group::Producer::demand` and `group::Demand` stay public (see
  Waiting).
- Settle whether `Producer::route` and `Request::route` refuse an instance that
  `is_minted()` (see Waiting).

## Plan

Found by the 2026-11-07 API shape audit (net model and net session); see
the [epic](/quest/m2/api-shape/README.md). The rest of the audit's items landed in
[moq-dev/moq#8266](https://github.com/moq-dev/moq/pull/8266). Start the un-hide
once #8063 merges. The `RouteEvent` reshape replaces
[moq-dev/moq#7862](https://github.com/moq-dev/moq/pull/7862)'s accessors, which
closed without taking it.

## Waiting

- Maintainer: decide whether `group::Demand` stays public. It cannot simply go
  `pub(crate)`: a paced flight needs it. The audit assumed flights stop through
  `Fill::poll_unwanted`, but that resolves as soon as readers take the group
  (`fills.wants`), while a paced flight has to see its riders leave;
  `rs/moq-net/tests/integration/flights.rs` and `rs/moq-net/benches/group.rs`
  call `group::Producer::demand` for that. Options: (a) keep
  `group::Producer::demand` and `group::Demand` public, documented as how a
  paced flight sees its riders leave (recommended: the one spelling that works
  today); (b) fold reader demand into `Fill::poll_unwanted` and
  `Fill::is_wanted`, then make `group::Demand` `pub(crate)`, a behavior change
  for every flight; (c) make it `pub(crate)` and move the flight test and the
  group bench in-crate.
- Maintainer: decide whether a route refuses a minted instance. Refusing it in
  `Producer::route`, `Request::route`, and `route_prefix` (Rust and JS) breaks
  three tests outside moq-net that route `Instance::minted()` directly to stand in
  for a route a pre-lite-07 session minted: moq-mux's
  `follow::tests::a_named_instance_follows_a_minted_one` and @moq/watch's
  `relativeBroadcast` and include tests. Options: (a) refuse, and rewrite those
  tests to receive the route over a pre-lite-07 session, which needs a version
  argument on moq-mux's `container/test_util.rs` `Link` and a session test helper
  @moq/watch can import from @moq/net (recommended: the API then cannot claim a
  minted instance as stable); (b) keep accepting it, documenting that a minted
  instance proves nothing; (c) refuse it in Rust only, leaving JS as is.
