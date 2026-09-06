# [S] Plan: the route cold cost across the bindings

## Goal

A settled shape for how a route's `{warm, cold}` cost crosses `moq-ffi` and
the language wrappers, so observing a route and re-announcing it cannot
silently rewrite a truthful cold cost. Run `/plan-quest`; the settled plan
becomes the implementing quest that closes the issue.

## Plan

`MoqOriginProducer::announce(prefix, MoqRoute)` is the input and
`MoqAnnouncement.route` the output. `From<origin::Route>` reports
`route.cost.warm` and drops `cold`, while `TryFrom<MoqRoute>` calls
`with_cost(u64)`, which sets both halves. So a truthful `{warm: 0, cold: N}`
observed through `announced()` and announced again becomes
`{warm: 0, cold: 0}`, claiming to be the publisher. `route_order` still ranks
on cold, so an understated cold wins ties it should lose.

Candidates, none chosen:

- Add `cold` to the `MoqRoute` record. Additive for the generated bindings
  and no C break, but it touches `{py,swift,kt,go,dart}/` and
  `doc/lib/*`; an omitted cold could default to the warm value, which is right
  for a publisher seeding a production cost.
- Keep the scalar and make the asymmetry unrepresentable: separate the
  seed-a-production-cost input from the observe-a-route output so an observed
  route cannot be re-announced as-is.
- Document `MoqRoute.cost` as a production cost that must not be echoed. The
  weakest option; `CLAUDE.md` prefers unrepresentable over documented.

Decide against `dynamic(prefix, route)` and `broadcast::Producer::announce(route)`
on `dev`, the surface the bindings mirror (see
[#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md)).

## Related

- [#2933](https://github.com/moq-dev/moq/issues/2933) - the issue the implementing quest closes
- [#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md) - the bindings surface this rides on
