# Active/passive route health

## Goal

Endpoints and relays detect stalled delivery on an actively demanded track by
comparing it with a next-best route observing group/frame progress without
payload. The shared routing model automatically resumes through a working
alternate and keeps the track there until that route fails. This applies only to
broadcasts with an explicit matching epoch: epochless broadcasts cannot resume
and keep their existing Restart behavior.

## Plan

Decided with the maintainer, 2026-10-13:

- Ranked in m2, after clustering stability, federation, and P2P. Health
  design had to reproduce a stall that transport failure and ordinary
  reconnect do not recover, or the line would be abandoned. It did
  ([moq-dev/moq#7096](https://github.com/moq-dev/moq/pull/7096)):
  `rs/moq-net/tests/integration/route_health.rs` pins a silent kill that loses
  about `detection - max_delay` of media, and a payload stall on live sessions
  that is never recovered while `largest()` keeps pace.
- A route that dies without a close counts as such a stall until the QUIC
  idle timeout fires (decided 2026-10-20, from
  [#6062](https://github.com/moq-dev/moq/issues/6062)): a killed relay
  starves a same-epoch subscription until the QUIC idle timeout fires; since
  [#6183](https://github.com/moq-dev/moq/pull/6183) resume recovers only what
  max age still holds, so a max age below the timeout still loses media.

- Application-assigned connection priority remains ahead of additive cost.
  Deterministic hashing still chooses distinct equal routes; a new connection
  wins only when routes otherwise match. Stalled delivery is addressed by
  evidence, not a general newest-connection preference.
- Observe at most one eligible same-epoch standby at a time per demanded
  track, the next-best route, aggregating downstream readers. If
  active and standby provide insufficient evidence, bounded exploration must
  reach other eligible candidates; a shared failure must not hide a working
  third route. Silence alone still does not establish failure. Metadata watchers
  must not recursively
  create their own backups at every relay or multiply into a network-wide
  monitoring tree. Monitor at endpoints and relays from the first implementation.
- A new mode in whichever lite wip is current when it lands; it does not
  gate lite-07. The preferred API is `Subscription.meta: true`: a live stream of
  group/frame headers with payload omitted, so a standby notices a live
  failure such as a dead old connection whose keep-alives stopped. `largest()`
  alone cannot: on a payload-less standby it is only the SUBSCRIBE_OK
  snapshot. A one-shot TRACK_STATUS on a new route's announcement is racy,
  since a switch needs the candidate to stay meaningfully ahead. It is not
  PROBE or a full media subscription hidden locally.
- Prefer per-track failover. The existing track resume layer can support it,
  but shared broadcast source selection must be separated from per-track
  active/standby selection. The maintainer permits broadcast-level fallback
  if this proves disproportionately complex; explain the evidence if used.
  One stalled track never establishes whole-session failure.
- Automatically let a stalled preferred route yield to a healthy lower-ranked
  eligible route. Relative metadata progress is evidence to test a replacement,
  not proof it can deliver the needed media. Brief payload overlap is allowed
  while verifying a promotion; steady standby carries no payload.
- Never automatically return to a demoted preferred route (decided
  2026-11-02, reversing the 2026-10-13 decision to return after useful media
  demonstrated recovery). The track stays on the working route until that
  route fails, by transport failure or by the same stall evidence. Payload
  trials that let a recovered route win the track back, with uncapped or
  capped doubling backoff, were rejected: the track is already being
  delivered, and a return would trade a working route for one that just
  failed, at the cost of overlap, possible oscillation, and a backoff
  schedule. Preserve ordinary transport failures.
- No health standby, automatic resume, or recovery promotion for epochless
  broadcasts, even when the path, node, or connection looks unchanged.

Decided with the maintainer, 2026-11-02, from the measured stalls:

- Evidence: the standby is a `Subscription.meta: true` subscription on the
  existing SUBSCRIBE; "SUBSCRIBE_META" was shorthand, not a new message.
  Switch when the active's delivered media timestamp lags the standby's
  reported timestamp by more than the reader's max delay, while the reader
  demands. That budget is what the reader already accepts losing, so a
  standby that is normally a little faster never overrides application
  priority, and no per-route baseline or fraction constant is needed. A
  reader without a max delay uses a fixed 2s. Until the active delivers its
  first frame, its delivered timestamp is that of the first frame the reader
  still needs, as the standby reported it, so a route that never delivers is
  caught on the same budget. Never compare `largest()` values, and never
  count source silence or a reader that stopped reading.
- Exploration: when neither active nor standby shows progress for the
  reader's max delay (or the same 2s), the standby slot moves to the next
  eligible candidate, doubling the wait per move. That bounds exploration:
  one standby at a time, and moves that slow down geometrically. With every
  candidate quiet, the slot keeps cycling through the eligible candidates at
  the doubling wait and fails nothing, since silence is not failure; the
  track stays on the active until real progress resets the wait or demand
  ends. One constant drives both the switch and exploration; it replaces
  falling back to the publisher's max age.
- Standby choice: the next-best route. Exploration already bounds the cost of
  an upstream it shares with the active, so preferring the candidate with
  the fewest shared hops was rejected as extra machinery.

The epic's remaining work is end-to-end validation across a multi-hop relay
network and alternate sessions representing CDN/direct paths: stalled video moves while healthy audio remains,
control advances while payload stalls, a recovered preferred route does not
take the track back while the working route delivers, and both quiet routes
stay unknown rather than falsely failing.
Test epochless exclusion, an active/standby shared bottleneck with a healthy
third route, simultaneous relay decisions,
loss/reordering, cancelled demand, and stale/replayed progress. Wire these into
CI. Report actual media overlap, control traffic, wakes, switches, and memory
across tracks, candidate connections, and downstream readers. Extend the
[P2P harness](/quest/m2/p2p/harness.md) with real browser/native cases:
metadata-only standby, stalled payload with advancing control, same-epoch
promotion with bounded overlap, no failback while the working route
delivers, and epochless exclusion.
Add a route-health section to the routing concept page.

## Required

- [Metadata subscriptions](/quest/m2/route-health/meta.md) - the current lite wip and the shared model observe frames without payload in Rust and JS
- [Track route selection](/quest/m2/route-health/selection.md) - share broadcast identity/candidates while tracks choose sources independently
- [Automatic recovery](/quest/m2/route-health/recovery.md) - epoch-only promotion in endpoints and relays, staying on the working route

## Related

- [Cluster routing](/quest/m1/cluster-routing/README.md) - reachability and advertisement freshness, distinct from delivery health
- [Link quality](/quest/m2/link-quality.md) - optional metric repricing, not this local delivery-health decision
- [Routing guide](/quest/m1/routing-guide.md) - priorities, health, and epoch-only continuity explained together
