# [M] Prove active/passive delivery health

## Goal

A reviewed design and controlled-time fixtures define metadata-only progress,
local stall evidence, promotion, and recovery for epoch-bound tracks at both
endpoints and relays. Update the implementation quests with measured choices;
do not add speculative production code or choose arbitrary timing thresholds.

## Plan

The [line's decisions](/quest/m2/route-health/README.md) are settled. The
stall the line needs is reproduced in `rs/moq-net/tests/integration/route_health.rs`
on the same-epoch diamond `P -> O -> M1/M2 -> E`:

- A silent kill of `M1` costs about `detection - max_delay` of media, by whole
  groups. At the 10s default idle timeout a 2s budget loses 7 groups and a 4s
  budget 5; detection under the budget loses none.
- `M1` withholding payload while every session stays up (no upstream stream
  credit, no downstream credit, or group headers without frames) starves `E`
  for 30s with no error, no route change, and no recovery, while `W` reads
  every frame through `M2`. With headers only, `E`'s `largest()` keeps pace with
  `W`'s while every group fails as too old, so comparing largest positions
  finds nothing wrong.

Lowering the idle timeout shrinks only the first case; nothing transport-level
recovers the second. Recovery turns these fixtures into resumes through `M2`.

The remaining counterexamples (quiet publishers, backpressure, exploration,
requalification, metadata cost) need the metadata mode, so they belong to the
implementation quests' tests. The proposals below are pending the maintainer;
once settled, fold them into [Metadata subscriptions](/quest/m2/route-health/meta.md),
[Track route selection](/quest/m2/route-health/selection.md), and
[Automatic recovery](/quest/m2/route-health/recovery.md), then delete this quest.

- Evidence: a stall is the active's lag behind the standby, measured on
  payload the active delivered against positions the standby reported, growing
  past its own healthy baseline by one frame interval of the track while the
  reader demands it. The baseline matters: a standby that is normally faster
  must not override application priority. Never compare largest values, and
  never count source silence or a reader that stopped reading. A false trigger
  costs one bounded payload trial, so evidence may be sensitive.
- Framing: a metadata group stream says so in its own header, so a stream
  opened before a mode update is read under the mode it was opened in.
  Metadata FRAMEs keep timestamp and declared size and carry no bytes, so a
  real empty frame stays distinguishable. Promotion re-requests payload from
  the reader's first missing frame; observed metadata never fills a cache or
  advances a resume cursor.
- Aggregation: a relay pulls payload upstream only for payload demand, and
  serves metadata from a payload copy it already holds. Only payload demand
  creates a standby, so metadata observers never branch further. A standby is
  pinned to its candidate's session, not routed through the front.
- Exploration: the standby slot moves to the next eligible candidate only
  while the reader's demand is outstanding and neither active nor standby has
  shown progress for the reader's own max delay, doubling that wait per move
  until real progress resets it. Without demand nothing explores; without a
  max delay, fall back to the publisher's max age, or explore never.
- Requalification: a demoted preferred route becomes the standby. Once its
  metadata catches up, it trials payload from the next group boundary and wins
  back the track after delivering that whole group within its old healthy
  baseline. Each failed trial doubles the groups before the next one, and
  metadata alone never requalifies it.

## Related

- [Routes and announces](/quest/m1/cluster-routing/routes.md) - candidate identity, identity-named requests, and transit priority semantics
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - mode changes must preserve requested positions and demand
