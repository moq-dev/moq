# [L] Automatically recover stalled epoch tracks

## Goal

Endpoints and relays automatically move a stalled track to a verified working
same-epoch alternate, and keep it there until that route fails, while
preserving logical readers and bounded control/media overhead.
This includes a route that dies without a close: the track moves before the
QUIC idle timeout declares it dead.
Epochless broadcasts never enter this active/passive mechanism.

## Plan

Decided with the maintainer 2026-11-02; the line README records why.
Observe the next-best eligible same-epoch candidate with a `meta: true`
subscription, shared across downstream readers. While the reader demands the
track, switch when the active's delivered media timestamp lags the standby's
reported timestamp by more than the reader's max delay, or a fixed 2s for a
reader without one. Compare payload the active actually delivered against
timestamps the standby reported, never `largest()` values. Neither source
silence nor a reader that stopped reading or applies backpressure establishes
a route failure. At a relay the reader is the aggregated subscription, so
its budget is the largest downstream max delay the aggregate already
carries. Open question for this quest: when one aggregated reader has no
max delay, the aggregate has none either and falls to 2s, which may be
shorter than another reader's budget; decide whether such a reader should
instead leave the others' budget in force.

Exploration: while demand is outstanding and neither active nor standby
shows progress for that same budget, move the standby slot to the next
eligible candidate, doubling the wait per move until real progress resets
it. A healthy third candidate stays discoverable behind an upstream the
active and standby share, while only one steady standby is observed at a
time. Without demand nothing explores.

Health may exclude an otherwise preferred route from a track, allowing a
lower-priority or more expensive eligible route to serve it. Keep that evidence
local to the affected delivery choice, not a global route-price change or
publisher END. Do not close a shared connection because one track stalled.

Metadata progress justifies trying a candidate. Promote it to payload with
an update on the same subscription, from the reader's first missing frame,
and demonstrate useful delivery before retiring a still-usable active copy.
Brief overlap during promotion is approved; no steady duplicate payload
pull. Reuse resume's retained in-flight copies and deduplication.

Never automatically return the track to a demoted preferred route (decided
2026-11-02, reversing the 2026-10-13 line decision to return after useful
media demonstrated recovery). The track stays on the working route until
that route fails, by transport failure or by the same stall evidence; no
payload trials, requalification backoff, or permanent polling of idle
connections. Cancel checks with reader demand, terminal track state, epoch
replacement, and session closure. A newer same-epoch connection is not
automatically a healthier route.

`rs/moq-net/tests/integration/route_health.rs` pins today's stalls: a silent
kill loses about `detection - max_delay` of media, and a payload stall with live
sessions is never recovered, even while `largest()` keeps pace. Flip both into
resumes through the standby; detection under the reader's max delay loses no
group.

Mock-time tests cover a preferred route whose metadata advances but media
remains blocked, failed promotion, a standby that is normally faster by less
than the budget and so never takes the track, a recovered preferred route
that does not win the track back until the working route fails, a reader
without a max delay using the 2s default, a shared active/standby failure
with a healthy third route, bounded exploration when every candidate is quiet,
a quiet publisher, receiver backpressure, stale or replayed reports, an epoch
change, multi-hop simultaneous failover, and no
standby for epochless broadcasts. Run through Rust/JS interop and existing CI.
Measure tracks, routes, and readers on both axes; retain the idle-session
poll-count regressions and report overlap, switch counts, and memory.

Public API: expose only health policy/evidence consumers need, mirrored across
languages if public; settle consequential shape through quest-plan. Wire: uses
Metadata subscriptions, with no separate health-specific route protocol.

## Required

- [Metadata subscriptions](/quest/m2/route-health/meta.md) - progress-only standby and safe promotion
- [Track route selection](/quest/m2/route-health/selection.md) - independent copies and shared resume
