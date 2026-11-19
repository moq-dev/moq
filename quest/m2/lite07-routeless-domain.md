# [M] lite-07 lets a domain route below MoQ

## Goal

Before lite-07 freezes, show that two relays of one trusted domain can
exchange claims over lite-07 without either sending ROUTE, DEAD, or DRAIN,
so a domain whose reachability comes from a node layer underneath (see
[the node layer](/quest/m3/node-layer.md)) still interoperates on the frozen
wire. Either a test on real sessions shows it as shipped, or the PR names
the smallest wire change that would make it possible.

Non-goals: building a node layer, or changing how today's routing nodes
route.

## Plan

Decided 2026-11-18 (maintainer, /quest-plan on decoupling routing from
moq-net): ✅ check before the cut and don't hold it for the exploration
(rejected: holding the cut for the whole node-layer exploration, which
stacks another L on the cut's serial path for a result that may keep
today's design; cutting with no check, which risks a frozen wire that forces
in-domain peers to exchange routes, so a layered relay cannot interoperate
across the release window).

What to check. Today a neighbour is `Role::Routing` (routes exchanged),
`Role::Scoped` (claims with a chain, no routes), or `Role::Endpoint`
(`rs/moq-net/src/route/relay.rs`), and `Msg::Announce` already carries
claims with no route. The question is whether an in-domain peer can carry
claims without routes, with each claim still naming its announcing relay
and ordered by that relay's revision and floor: a scoped grant covering the
whole tree, a peer that announces itself as an endpoint, or neither.

Recommendation: a route-free in-domain claim needs the announcing relay's
identity, revision, and floor, which `Msg::Claims` carries, without
`Msg::Route`. If the shipped wire only allows that across a scoped grant,
measure what the chain costs and whether a whole-tree scope breaks anything.
If no shipped form works, the PR proposes the smallest change, such as
`Claims` being valid from a peer that never sent its route, and lists the
decision under `## Waiting` for the maintainer, since whether to hold the
cut for it is theirs.

## Related

- [The node layer](/quest/m3/node-layer.md) - the exploration this keeps open
- [Finalize moq-lite-07](/quest/m2/lite07-finalize.md) - the cut this must precede
- [Cache-aware multicast](/quest/m2/multicast/README.md) - the route layer as shipped
