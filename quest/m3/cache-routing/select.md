# [XL] Relays pull each broadcast from the cheapest copy

## Goal

Relays learn of warm copies the way
[warmth](/quest/m2/multicast/warmth.md) picked, and each pulls every demanded
broadcast from the cheapest copy of its Instance, moving an existing pull
only when the route it uses gets worse or ends, or another is strictly
cheaper in summed cost. A peer that sends no warm signal is treated as cold
and behaves as it does today. Relays export how many upstream copies they
hold and how many moves a warm copy caused.

## Plan

Rewritten 2026-11-15 (maintainer, /quest-plan on
[cache-aware multicast](/quest/m2/multicast/README.md)): the rule this was
to build, item 14 of the [epic](/quest/m3/cache-routing/README.md)'s earlier
Plan, is replaced. The warmth quest's PR fills in the mechanism here once
the maintainer has picked it.

What holds whichever is picked:

- A warm link costs its warm price and a cold one its cold price, both at
  least 1, summed from the source along the path the data takes.
- Extend today's selection rather than adding a second selector.
  `route_order` in `rs/moq-net/src/model/origin.rs` already hashes equal
  routes per path, and a front already moves to a better route of its own
  Instance and resplices (`rs/moq-net/src/model/front.rs`). A front moves
  only to a copy of its own Instance.
- The rule runs in production code that the fixtures and sweeps drive. The
  stripped rule lived only in a test simulator that checked it against
  itself, so a rule that never offered a copy passed.
- Check the Goal's efficiency: copies held against plain path routing.
- Bound the warm state a peer can make a relay hold by design.
- Export upstream copies held and moves by cause through the relay's
  existing metrics.
- `rs/moq-net/AGENTS.md` says a relay behaves the same warm or cold. That
  covers answers to requests; this changes which neighbour a request goes
  to. Bring a rewording to the maintainer if the two contradict.

Decided 2026-11-14 (maintainer, pick on
[moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ lite-07
freezes without a warm wire, so whatever wire the pick needs lands in the
then-current wip version with this.

Public API: report any relay configuration added, such as a warm cost per
link. Wire: whatever the pick needs, in the then-current wip version.

## Required

- [Warmth](/quest/m2/multicast/warmth.md) - picks the mechanism this builds
- [The switch](/quest/m2/multicast/switch.md) - the route layer it ranks on

## Related

- [Route health](/quest/m3/route-health/README.md) - its metadata-only subscription is the query if the pick is to ask on subscribe
