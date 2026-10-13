# Active/passive route health

## Goal

Endpoints and relays detect stalled delivery on an actively demanded track by
comparing it with a next-best route observing group/frame progress without
payload. The shared routing model automatically resumes through a working
alternate and returns to a recovered preferred route. This applies only to
broadcasts with an explicit matching epoch: epochless broadcasts cannot resume
and keep their existing Restart behavior.

## Plan

Decided with the maintainer, 2026-10-13:

- Application-assigned connection priority remains ahead of additive cost.
  Deterministic hashing still chooses distinct equal routes; a new connection
  wins only when routes otherwise match. Stalled delivery is addressed by
  evidence, not a general newest-connection preference.
- Observe at most one eligible same-epoch standby at a time per demanded
  track, initially the next-best route, aggregating downstream readers. If
  active and standby provide insufficient evidence, bounded exploration must
  reach other eligible candidates; a shared failure must not hide a working
  third route. Silence alone still does not establish failure. Metadata watchers
  must not recursively
  create their own backups at every relay or multiply into a network-wide
  monitoring tree. Monitor at endpoints and relays from the first implementation.
- New lite-07 support is required. The preferred API is `Subscription.meta:
  true`: group/frame progression with payload omitted. It is not PROBE,
  repeated TRACK_STATUS snapshots, or a full media subscription hidden locally.
- Prefer per-track failover. The existing track resume layer can support it,
  but shared broadcast source selection must be separated from per-track
  active/standby selection. The maintainer permits broadcast-level fallback
  if this proves disproportionately complex; explain the evidence if used.
  One stalled track never establishes whole-session failure.
- Automatically let a stalled preferred route yield to a healthy lower-ranked
  eligible route. Relative metadata progress is evidence to test a replacement,
  not proof it can deliver the needed media. Brief payload overlap is allowed
  during promotion and recovery verification; steady standby carries no payload.
- Automatically return to the preferred route after useful media delivery
  demonstrates recovery. Metadata alone does not requalify it. Establish
  evidence and anti-oscillation rules from controlled failures, not an arbitrary
  delay or a blanket transport reconnect. Preserve ordinary transport failures.
- No health standby, automatic resume, or recovery promotion for epochless
  broadcasts, even when the path, node, or connection looks unchanged.

The epic's remaining work is end-to-end validation across a multi-hop relay
network and alternate sessions representing CDN/direct paths: stalled video moves while healthy audio remains,
control advances while payload stalls, the recovered primary resumes without
oscillation, and both quiet routes stay unknown rather than falsely failing.
Test epochless exclusion, an active/standby shared bottleneck with a healthy
third route, simultaneous relay decisions,
loss/reordering, cancelled demand, and stale/replayed progress. Wire these into
CI. Report actual media overlap, control traffic, wakes, switches, and memory
across tracks, candidate connections, and downstream readers. Real browser/native
P2P health integration belongs to the P2P harness, which Requires Automatic
recovery; this line can complete without waiting for P2P transports.

## Required

- [Health design](/quest/m1/route-health/design.md) - prove useful progress, aggregation, switching evidence, and bounded recovery
- [Metadata subscriptions](/quest/m1/route-health/meta.md) - lite-07 and the shared model observe frames without payload in Rust and JS
- [Track route selection](/quest/m1/route-health/selection.md) - share broadcast identity/candidates while tracks choose sources independently
- [Automatic recovery](/quest/m1/route-health/recovery.md) - epoch-only promotion and evidence-based failback in endpoints and relays

## Related

- [Cluster routing](/quest/m1/cluster-routing/README.md) - reachability and advertisement freshness, distinct from delivery health
- [Live largest location](/quest/m1/largest.md) - reported source position is not delivered payload
- [Link quality](/quest/m2/link-quality.md) - optional metric repricing, not this local delivery-health decision
- [Routing guide](/quest/m1/routing-guide.md) - priorities, health, and epoch-only continuity explained together
