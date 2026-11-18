# [M] A relay counts what its route layer does

## Goal

From a relay's `/metrics` and its stats, an operator rolling a mesh onto
moq-lite-07 can tell, per peer label and without reading logs:

- how often its routes change, and how many broadcasts each change rewrote
  into the origin;
- how many withdrawals it sent and received, DEAD and DRAIN apart;
- why it refused a route or a claim, by kind, where `routes_refused` gives
  one number today;
- whether a peer's announce queue fell behind or overran;
- and how many sessions it closed for a route error, by kind.

`doc/concept/stats.md` says what a rise in each one means during a rollout.

## Plan

Decided 2026-11-18 (maintainer, in the coordinating session, from a survey of
the moq.pro deployment): ✅ planned (rejected: leaving it out). The route
layer's only signal in a relay's stats is `routes_refused`, and `/metrics`
has announce and control counters and nothing for routes. moq.pro judges a
canary relay by its counters against its neighbours', so a route layer that
churns or disagrees between releases is invisible to the roll that would
catch it.

Decided 2026-11-18 (maintainer, on
[moq-dev/moq#9110](https://github.com/moq-dev/moq/pull/9110)): ✅ it blocks
the lite-07 cut, under this epic, with
[the mesh replay](/quest/m2/multicast/mesh-replay.md) that reads these
(rejected: m3). The release a deployment rolls onto lite-07 then carries
them.

Agent's choices, for the implementer to revisit:

- Count in `rs/moq-net/src/stats.rs` beside `routes_refused`, per label and
  never per broadcast or per node, so the cost and the output stay flat as
  the mesh grows.
- The count of broadcasts rewritten is the cost the `routes/reopen` bench
  measures and [route wakes](/quest/m4/route-wakes.md) owns, read from a
  running relay.
- Say what the announce counters mean on a lite-07 session, where a route
  change is no longer one announce per broadcast, and fix the doc or the
  counter if they now mislead.

Public API: stats fields and metric names, additive; nest them under their
shared role. Wire: none.

## Related

- [Real relays replay a mesh and its faults](/quest/m2/multicast/mesh-replay.md) - reads these in its report
