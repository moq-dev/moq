# [L] A withdrawn claim ends across mixed lite-06 and lite-07 relays

## Goal

After a publisher leaves, its broadcast ends at every relay within a bounded
number of route updates, for any mix of moq-lite-06 and moq-lite-07 relay
links. That includes a rolling upgrade, where one lite-06 relay links to
three or more lite-07 relays. Today such a claim circles forever after its
withdrawal, and every subscriber sees a Restart on every lap.

Not here: relay links over moq-transport. moq-transport is supported for
clients, not relays (decided 2026-11-08), so its anonymous chains stay out of
scope.

## Plan

Root cause, reproduced 2026-11-08 on main at `8065e7148f`. The loop has been
on main since [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023),
which added the lite-06 translation and its single-link reflection check:

- A route learned over lite-06 becomes the translating relay's own claim
  (`OwnClaims::apply` in `rs/moq-net/src/model/origin.rs`, the `learned`
  flag), and its lite-06 hop chain is dropped there.
- Translating a lite-07 route back out to lite-06 sends only its origin
  node's hop (`hops()` in `rs/moq-net/src/lite/subscriber/routed.rs`).
- So `Subscriber::chain` (`rs/moq-net/src/lite/subscriber.rs`) only catches a
  claim that went out and came back across one lite-06 segment. Once a claim
  crosses two lite-06 to lite-07 boundaries, no relay sees its own hop again.
  In A-07-B-06-C-07-D-06-A, B and D keep re-originating each other's copy.
- Minting a `~` instance per lite-06 hop only turns each lap into a Restart.
  It does not cause the loop, and the rising cost saturates only near
  `MAX_COST`, so in practice the loop never ends.

Announcing settles in every topology probed; only withdrawal loops. It loops
in a ring with two lite-06 links separated by lite-07 links (three behave the
same), and when one lite-06 relay links to three or more lite-07 relays,
wherever the publisher sits. A 07-06-06 triangle, a ring with adjacent lite-06
links, and an old relay with two lite-07 neighbours all settle.

Decided 2026-11-08 (maintainer, /quest-plan):

- ✅ Carry the lite-06 chain through lite-07, as a crate-private chain plus a
  lite-07-wip compatibility extension and field. Rejected: a cost or hop
  ceiling with no wire change, which still circles for hundreds to thousands
  of laps and is an arbitrary constant; never forwarding a lite-06-learned
  claim to another lite-07 relay, which breaks old-new-new chains mid-upgrade;
  documenting mixed rings as unsupported, which a rolling upgrade cannot
  avoid. A claim learned over lite-06 keeps that chain. lite-07
  ANNOUNCE_START, ANNOUNCE_RESTART and ANNOUNCE_UPDATE carry it, empty for a
  native claim. Translating back to lite-06 sends the chain plus this node's
  hop. Both receive sides drop a chain that names them: lite-06 in `chain()`,
  and lite-07 when it serves the route. `max_hops` then bounds the total
  across islands, and the existing route-hop check becomes one case of it.
- ✅ It is temporary: the draft and code mark the extension and field as
  lite-06 compatibility, and the lite-07 to lite-08 migration deletes them.
  No public API carries the chain. Keep it apart from the minted flag that
  `quest/m2/instance-provenance.md` plans (on
  [moq-dev/moq#8266](https://github.com/moq-dev/moq/pull/8266)'s branch
  until it merges), even though a non-empty chain marks exactly the
  announcements that flag would: the flag also covers moq-transport routes
  and outlives lite-06.
- ✅ m1 (rejected: m0, m2). lite-07 is opt-in `-wip`, so nothing published
  breaks, and the field must exist before
  [lite-07 is finalized](/quest/m2/lite07-finalize.md).

A throwaway prototype, with a process-wide table standing in for the wire
field, made every looping topology settle in 4 to 13 events after withdrawal.
All integration and lib tests still passed. It needed the lite-07-side drop,
or it tripped `origin.rs`'s "announce called with a looping hop chain"
debug assertion. A chain change on an open session also needs
ANNOUNCE_UPDATE to carry the field.

Tests, in `rs/moq-net/tests/integration/mesh_withdraw.rs` beside
`mixed_version_ring_withdraw_retracts`. Each announces at one relay, checks
that every relay sees it start, withdraws it, and checks that every relay
sees it end:

- The ring A-07-B-06-C-07-D-06-A.
- A rolling upgrade with the publisher at the lite-06 relay, linked to each
  relay of a three-relay lite-07 full mesh. This one hung 12 of 12 runs, so
  it is the reliable gate.
- The same rolling upgrade with the publisher at a lite-07 relay. It hung 11
  of 12 runs, because route identities are random and tie-breaks hash them.
  Make it deterministic or drop it.

The js/net lite-07 port in [routes/js.md](/quest/m2/cluster-routing/routes/js.md)
decodes the field; JS translates nothing, so it keeps no chain.

[moq-dev/moq#8293](https://github.com/moq-dev/moq/pull/8293) keeps hop chains
out of the public API, so the chain this carries is a third crate-private
source of them, and gives an origin one identity on both wires, which folds
the two receive checks together.

## Related

- [Routes acceptance](/quest/m2/cluster-routing/routes/acceptance.md) - these tests are its kind of fixture, a stale neighbour unable to revive an ended broadcast
