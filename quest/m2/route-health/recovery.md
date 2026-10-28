# [L] Automatically recover stalled epoch tracks

## Goal

Endpoints and relays automatically move a stalled track to a verified working
same-epoch alternate, then return to the preferred route after demonstrated
recovery, while preserving logical readers and bounded control/media overhead.
This includes a route that dies without a close: the track moves before the
QUIC idle timeout declares it dead.
Epochless broadcasts never enter this active/passive mechanism.

## Plan

Implement the evidence rules established by Health design. Observe the next-best
eligible same-epoch candidate using metadata-only demand, shared across downstream
readers. Apply the proven bounded exploration rule when that standby is
uninformative, so a healthy third candidate remains discoverable while only
one steady standby is observed at a time. Compare remote progress, actual
useful payload, and outstanding demand;
neither source silence nor local backpressure alone establishes a route failure.

Health may temporarily exclude an otherwise preferred route, allowing a
lower-priority or more expensive eligible route to serve. Keep that evidence
local to the affected delivery choice, not a global route-price change or
publisher END. Do not close a shared connection because one track stalled.

Metadata progress justifies trying a candidate. Promote to payload at the
needed position and demonstrate useful delivery before retiring a still-usable
active copy. Brief overlap during promotion/recovery is approved; no steady
duplicate payload pull. Reuse resume's retained in-flight copies and deduplication.

Automatically requalify a recovered preferred route using real delivery,
not metadata alone, and restore ordinary priority/cost ordering. Follow the
measured anti-oscillation rule; no unbounded retry loop, permanent polling of
idle connections, or arbitrary hold-down as a correctness substitute. Cancel
checks with reader demand, terminal track state, epoch replacement, and session
closure. A newer same-epoch connection is not automatically a healthier route.

Mock-time tests cover all Health design counterexamples, including a preferred
route whose metadata advances but media remains blocked, failed promotion,
recovery that repeatedly degrades, a shared active/standby failure with a
healthy third route, bounded exploration when every candidate is quiet,
multi-hop simultaneous failover, and no
standby for epochless broadcasts. Run through Rust/JS interop and existing CI.
Measure tracks, routes, and readers on both axes; retain the idle-session
poll-count regressions and report overlap, switch counts, and memory.

Public API: expose only health policy/evidence consumers need, mirrored across
languages if public; settle consequential shape through quest-plan. Wire: uses
Metadata subscriptions, with no separate health-specific route protocol.

## Required

- [Metadata subscriptions](/quest/m2/route-health/meta.md) - progress-only standby and safe promotion
- [Track route selection](/quest/m2/route-health/selection.md) - independent copies and shared resume
