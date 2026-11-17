# [M] Held after close

## Goal

A relay whose own session to its next hop closes, leaving it no usable
route, shows its claims held and unavailable to its own readers, as its
neighbours already do, instead of offline followed by a fresh Start once the
route recovers.

## Plan

Reordered 2026-11-15 (maintainer, /quest-plan on [cache-aware
multicast](/quest/m2/multicast/README.md)): this waits on the
[bake-off](https://github.com/moq-dev/moq/pull/8880). The new rule has no hold: a reader
debounces unavailable to offline locally, which answers the same symptom. If
the new rule goes ahead, [the switch](/quest/m2/multicast/switch.md) deletes
this quest with the fixtures that pin it; if it does not, this starts as
written.

Kept 2026-11-15 (maintainer, /quest-complete on
[moq-dev/moq#8832](https://github.com/moq-dev/moq/pull/8832)): ✅ keep it in
m2 (rejected: drop; moving it to m3).

Found by [acceptance](/quest/m2/cluster-routes/acceptance.md) on real
lite-07 sessions, three times over:
`route_acceptance::a_ring_waits_out_the_doubt_for_a_backup_through_the_far_end`,
`route_acceptance::a_primary_that_fails_and_returns_takes_every_relay_back`,
and `route_cluster::a_partitioned_lan_reaches_itself_through_the_cdn_and_heals`
pin it. The router holds the route correctly; the relay's readers see
Offline at the cut, then Start about 2 s later, after the origin's
retraction quarantine. Subscriptions end and a fresh one reads on.

The cause is in `rs/moq-net/src/route/node.rs` (`Node::close` and
`State::close`): a held claim's only slot in the origin was on the closed
session's stream, and closing the stream withdraws everything it served, so
nothing is left to show the claim unavailable.

Recommended: when a held claim loses its last slot to a closing stream,
move it, unavailable, onto a stream that still hears the held route, or show
it through the node itself, before the closing stream withdraws it. Turn the
three pinned fixtures into the contract's held-then-available sequence.

## Required

- [The switch](/quest/m2/multicast/switch.md) - deletes this with the old rule, unless the maintainer keeps today's rule on [moq-dev/moq#8880](https://github.com/moq-dev/moq/pull/8880)


## Related

- [Routes and announces](/quest/m2/cluster-routes/README.md) - the hold rule and "Presence and availability are separate"
