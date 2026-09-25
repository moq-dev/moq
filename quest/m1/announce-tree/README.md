# Conservative announcement pruning

## Goal

Relays avoid sending announcements that are strictly worse than usable routes
already available to the receiver. Reduce total cluster control traffic and
held route copies without changing route selection, losing reachable prefixes,
or breaking an existing subscription's source identity. Unknown or unsupported
cases keep today's flooding.

Measure starts, ends, updates, bytes, and held copies against flooding on the
same workload. Include reachability, pruning control, and reconvergence costs.
There is no fixed two-copy or per-event message bound: equal-ranked paths,
multiple sources, retained backups, mixed versions, and handoffs can require
more copies. A retained announcement does not imply an active media path or
gapless playback.

## Plan

Amortizing reachability over all broadcasts from a source relay is the preferred
starting point. Relay reachability alone does not prove that a neighbor holds
or will send a particular prefix. Prototype the pruning and recovery policy in
the simulator before committing to its wire representation.

The first version preserves the existing ranking and equal-ranked alternatives.
It prunes only with evidence valid for the receiver's scope, source identity,
and current sessions. It retains routes needed by the chosen backup policy.
If that evidence is missing, invalidated, or cannot represent a route, forward
normally. Healthy handoffs establish replacement announcements before retiring
old ones; temporary overlap is allowed.

Backup availability must be checked against actual prefix routes. A neighbor
may prefer another source, or choose a different equal-cost path for a
broadcast than it advertised for relay reachability. Neither a different
neighbor nor a reachability chain avoiding the primary proves node protection.
Preserving all failure-path behavior is not implied by preserving the current
best route: the policy quest must state which standbys remain and how others
are rediscovered. Any recovery tradeoff needs a maintainer decision before
shipping; do not hide it behind a copy target.

Start with authenticated lite-07 cluster sessions. Customers, older Lite
versions, and IETF sessions keep flooding. A mixed mesh must remain complete,
including upgraded components connected through an older relay. Broadcast
announcements and ANNOUNCE_REQUEST need not change unless the validated policy
requires it; the cluster-stream quest owns the precise wire impact.

Non-goals: interest-based discovery, a new global tie-break order, and guaranteed
gapless media failover. Every relay still learns the cluster's reachable
prefixes. The [prefix table](/quest/m2/announce-prefix-table.md) reduces bytes
per message independently.

## Quests

- [Announce counters](/quest/m1/announce-tree/counters.md) - establish the flooding baseline
- [Cluster simulator](/quest/m1/announce-tree/simulator.md) - exercise real origins and adversarial delivery before choosing a wire format
- [Pruning policy](/quest/m1/announce-tree/policy.md) - validate the suppression evidence, standby policy, and recovery transitions
- [Cluster stream](/quest/m1/announce-tree/cluster-stream.md) - encode the validated control state on authenticated cluster sessions
- [Relay reachability](/quest/m1/announce-tree/reachability.md) - maintain and expose the evidence without suppressing announcements
- [Pruned forwarding](/quest/m1/announce-tree/forward.md) - apply the policy and measure its total cost against flooding

## Related

- [Relay memory](/quest/m1/relay-memory.md) - remeasure held copies and their allocation cost
- [PoP skipping](/quest/m1/pop-skipping/README.md) - weighted links and warm exact-path routes are policy test cases
- [Stats linger](/quest/m1/stats-linger.md) - removes a churn source independently of forwarding
