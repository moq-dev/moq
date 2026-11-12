# [M] Widening an interest ends no live claim

## Goal

A client that widens its interest never sees END for a claim its publisher
still has. `sweep_scoped_handoff` in `rs/moq-net/src/route/tests/handoff.rs`
reports no false END over 20,000 scenarios, and the case below is a plain
test.

## Plan

Found 2026-11-11 by running the sweep past its 150 scenarios: 25 of 20,000
end a live claim, the first at scenario 750, on `main` at `688312b9eb`. It is
the same whichever neighbours the router believes, so
[the route per scope](/quest/m2/cluster-routing/routes/scope.md) does not fix
it. Decided the same day (maintainer, /quest-plan): ✅ plan it (rejected:
leaving it to the interest set).

Scenario 750, shrunk to 13 steps. The client sits behind two relays, each
under a grant for one prefix, and sees the publisher's `live` at whichever
prefix its interest and a grant meet. It narrows to `live/b/z`, then widens
to everything while its sessions and the publisher's flap. Its reader sees
`live/b/z` start, go offline, start, and END, where the claim only moved up
to `live/b`:

```rust
let (origin, r1, r2, client) = (0, 1, 2, 3);
let mut sim = Sim::new(4);
sim.leaf(origin);
sim.leaf(client);
sim.join(origin, r1, 1);
sim.join(origin, r2, 1);
sim.join(r1, r2, 1);
for (relay, scope) in [(r1, "live/a/"), (r2, "live/b/")] {
	let grant = Grant { subject: "local", scope };
	sim.link(client, relay, SimLink::cost(1).grant(grant), SimLink::cost(1));
}
sim.originate(origin, 7, 0);
assert!(sim.run());

sim.announce(origin, "live/", 0, None);
sim.settle(1);
sim.deliver(r1, origin);
sim.settle(1);
sim.disconnect(client, r1);
sim.settle(1);
sim.disconnect(origin, r1);
sim.settle(1);
sim.deliver(r2, r1);
sim.settle(1);
sim.disconnect(origin, r2);
sim.settle(1);
sim.connect(origin, r1);
sim.settle(1);
sim.watch(client, "live/b/z");
sim.settle(1);
sim.disconnect(origin, r1);
sim.settle(1);
sim.announce(origin, "live/", 1, None);
sim.settle(1);
sim.connect(origin, r2);
sim.settle(1);
sim.watch(client, "");
sim.settle(1);
sim.disconnect(client, r2);
sim.settle(1);

sim.connect(origin, r1);
sim.connect(client, r1);
sim.connect(client, r2);
assert!(sim.run());
assert_eq!(false_ends(&sim.events), 0);
```

`show` in `route/mod.rs` reports a claim that left as offline, not ended,
when a claim above it with the same item is still shown. Here that check
missed. Find why before changing it: the order the widened stream's first
batch and the session close arrive in is the likely cause.

- Shrink further if it helps, then fix the cause and keep the case as a
  test.
- Give the sweep a way to run more scenarios than the gate's 150 without
  editing it, and say in the PR how many it ran clean.
- [The interest set](/quest/m2/cluster-routing/routes/interest-set.md)
  replaces "an interest change opens a new stream" with an edit in place. If
  it lands first, run the case and the 20,000 scenarios on it, and delete
  this quest if both are clean.

Public API: none. Wire: none.

## Related

- [The route model and router take an interest as an edited set of prefixes](/quest/m2/cluster-routing/routes/interest-set.md) - rewrites interest changes, and may remove the case
