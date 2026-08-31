# [M] moq-ffi: MoqRoute round-trip erases a route's cold cost

## Goal

Implement and verify the behavior tracked in [#2933](https://github.com/moq-dev/moq/issues/2933)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Found by the Codex reviewer during [#2925](https://github.com/moq-dev/moq/pull/2925), which splits the route cost into `broadcast::Cost { warm, cold }`. Filing rather than fixing in that PR because it is a public-API decision spanning five language bindings.

#### The gap

`rs/moq-ffi/src/origin.rs` maps the pair onto one scalar in each direction:

- `From<broadcast::Route> for MoqRoute` reports `route.cost.warm` and drops `cold`.
- `TryFrom<MoqRoute> for broadcast::Route` calls `with_cost(u64)`, which sets *both* halves to that scalar.

So a caller that observes a route through `MoqBroadcastConsumer::route_updates` and feeds it back into `MoqBroadcastProducer::set_route` converts a truthful `{warm: 0, cold: N}` into `{warm: 0, cold: 0}`. Cold 0 means "I am the publisher", so the republished route understates how far the content actually is, and a relay forwarding it advertises a cold cost lower than the truth.

This was deliberately scoped out of #2925 to avoid rippling `cold` through `libmoq`, `py/`, `swift/`, `kt/`, `go/`, and `doc/lib/*` for a field no application routes on. The round-trip being silently lossy in the *unsafe* direction is what makes it worth revisiting.

#### Severity is narrower than it first looks

The handover gate only consults an advertised cold cost when `hops.len() >= 2` (see `FrontState::handover_allowed`). A route published by an application arrives at its relay with a one-hop chain, so it is treated as a direct publisher route and never enters the rank comparison. An application can also already misreport its production cost through the existing scalar, so this is not a new trust boundary  -  the relay mesh has always taken a publisher's seeded cost at face value.

What is new is that the *observe then republish* path silently rewrites a value the app did not choose, rather than the app choosing to lie.

#### Options

1. Add `cold` to the `MoqRoute` uniffi record. Additive for the generated bindings (`rs/libmoq` does not expose routes over the C ABI, so no C break), but per the Cross-Package Sync table it touches `{py,swift,kt}/`, `go/wrapper/moq/*.go`, and `doc/lib/{py,swift,kt,go}`. An omitted cold could default to the supplied warm value, which is correct for a publisher seeding a production cost.
2. Keep the surface scalar and make the asymmetry unrepresentable instead, e.g. by separating the seed-a-production-cost input from the observe-a-route output so an observed route cannot be passed to `set_route`.
3. Accept it and document `MoqRoute.cost` as a production cost that is not meant to be echoed. Weakest option; `CLAUDE.md` prefers making misuse unrepresentable over documenting it.

## Closes

- [#2933](https://github.com/moq-dev/moq/issues/2933) - close this issue when the quest finishes
