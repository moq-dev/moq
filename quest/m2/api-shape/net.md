# [S] moq-net's route stream goes public

## Goal

The rest of moq-net's small surface fixes, with their JS mirrors:

- Un-hide `origin::Consumer::routes`, `Routes`, and `RouteEvent`, documented
  as the relay-level stream that moq-relay and downstream relays already
  consume, and fix `request`'s doc to say what
  [moq-dev/moq#8063](https://github.com/moq-dev/moq/pull/8063) made it do.
  `RouteEvent` becomes `pub struct RouteEvent { pub kind: RouteKind, pub
  announce: Announce }` with `pub enum RouteKind { Start, Update, Restart, End }`,
  mirroring JS's `{ kind } & Routed`, so callers need no or-pattern and no
  accessors.

## Plan

Found by the 2026-11-07 API shape audit (net model and net session); see
the [epic](/quest/m2/api-shape/README.md). The rest of the audit's items landed in
[moq-dev/moq#8266](https://github.com/moq-dev/moq/pull/8266). The un-hide
waited on #8063 removing `RouteEvent::Takeover`; #8063 has merged, so it is
unblocked. The `RouteEvent` reshape replaces
[moq-dev/moq#7862](https://github.com/moq-dev/moq/pull/7862)'s accessors, which
closed without taking it.

Decided 2026-11-08 (maintainer): ✅ `group::Demand` and
`group::Producer::demand` stay public, documented for paced flights
(rejected: fold reader demand into `Fill::poll_unwanted` then make it
private; make it private and move the flight test and bench in-crate).
`Fill::poll_unwanted` resolves as soon as readers take the group, while a
paced flight has to see its riders leave; `tests/integration/flights.rs` and
`benches/group.rs` use it for that. Its docs say so since
[moq-dev/moq#8266](https://github.com/moq-dev/moq/pull/8266).

Decided 2026-11-08 (maintainer): whether a route refuses a minted instance
is replaced by [Minted instances are a typed flag](/quest/m2/instance-provenance.md).
The `~` prefix is a spoofable in-band sentinel; once only a session can mint
an instance, the route needs no refusal.

## Related

- [Minted instances are a typed flag](/quest/m2/instance-provenance.md) -
  replaces this quest's minted-route refusal item
