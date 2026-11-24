# [M] Cache fair share across tenants

## Goal

One tenant cannot crowd another out of the relay's shared group cache. With a
cache capacity set, a tenant that opens many sessions, broadcasts, or tracks,
or stops writing while holding cached bytes, keeps no more than its fair
share once another tenant needs room. Recently read content of a light tenant
survives a heavy tenant's writes. Single-tenant relays behave as today.

Non-goal: a default byte capacity. An unbounded pool with no knobs set is a
generous default the operator can lower, which `.oss-scanner/threat_model.md`
classes as not a vulnerability.

## Plan

From the 2026-10-28 external security audit ("Cache memory is unbounded by
default"), checked at
[7ea27dd4d9](https://github.com/moq-dev/moq/commit/7ea27dd4d9). The threat
model's multi-tenancy boundary says one tenant must not degrade another
"by exhausting a shared cache", and no quest owned it.

Today a relay shares one `cache::Pool` (`rs/moq-net/src/model/cache.rs`)
across every origin. Over capacity, each track accrues eviction debt in
proportion to what it writes and pays by aborting its own oldest groups.
Ordering across tracks comes from the mean last-access time, and every live
track's latest group is exempt from eviction and expiry. So bytes are shared
per writing track, not per tenant: a tenant with more tracks gets more
latest-group exemptions, and a track that stops writing pays nothing down
under byte pressure (`rs/moq-relay/src/cache.rs` documents this). Per-session
limits (`rs/moq-relay/src/limits.rs`) bound each session, not a tenant's
total across sessions.

Decided 2026-10-28 (maintainer): measure first, then propose. Waits for
[unbounded groups](/quest/m3/unbounded-groups/README.md), which bounds each
group copy at a 32 MB tail and lets front eviction shed a live latest group, so
the measurement runs against the final eviction rules. Show with a test
whether a heavy tenant (many sessions and tracks, or a stopped publisher)
evicts a light tenant's recently read groups or holds more than an even share
under a set capacity. If it does, bring options with a recommendation, such
as per-tenant accounts in the pool with debt charged to the tenant over its
share, before building.

Open, for the design: what keys a tenant, such as the grant root, which
exists today. Pick the same key as
[broadcast fairness](/quest/m5/broadcast-fairness.md)'s tenant keying.

Decided 2026-11-22 (/quest-plan on evicting the latest group and reordering
the LRU; the maintainer confirmed that goal, then asked the agent to judge
whether it was worth doing): ✅ no eviction redesign now, folded into this
quest's measurement (rejected: a redesign quest of its own; a narrower quest
that only drops the latest-group exemption). The reasons:

- With the answers below, the target order is what `pay_debt`
  (`rs/moq-net/src/model/track.rs`) already does: the pool's mean last access
  is one cutoff, and each track evicts its own groups below it as it writes.
- A track that is writing already sheds the front of its latest group to pay
  its debt. What stays pinned is the latest group of a track that is not
  writing: at most 32 MB each, and 256 MiB per client session since
  [moq-dev/moq#9308](https://github.com/moq-dev/moq/pull/9308) (`live_edges`).
  Two sums stay unbounded: over sessions, and over a peer link, which carries
  no budget.
- Reclaiming those means a writer evicting another track's latest group. That
  gives up what #9308 chose the day before: no session's writes touch another
  session's live edge. Under a recency-only order, a busy publisher's writes
  would push out a quiet publisher's catalog.
- Nothing measured shows quiet latest groups holding a meaningful share of a
  relay's memory.

So the measurement above also reports what latest groups pin against the
pool's target, in the two unbounded cases: a relay fed by a peer link whose
subscribers hold many quiet tracks, and many client sessions each inside its
`live_edges` budget. If either holds the pool well over its target, bring the
design below with the options.

Settled for that design, so it is not asked again:

- Order: ✅ recency only (rejected: track priority first, which evicts video
  being watched to keep idle audio and evicts last a publisher that marks
  every track top priority; priority as a tie-break between groups of similar
  age, which the maintainer called niche and able to starve).
- Trigger: ✅ writes only, with no new timer or sweep, which the maintainer
  dislikes. Retiring the idle sweep itself was not planned.
- Producer: ✅ a track's original producer keeps its latest group, since no
  upstream can refill it, so only a copy with an upstream is cache (rejected:
  evicting everywhere, which can lose a catalog for good).
- [moq-dev/moq#9422](https://github.com/moq-dev/moq/pull/9422) ✅ lands as it
  is (rejected: holding it for a redesign). A pool that reclaims latest groups
  shrinks its `pinned` count to the groups producers hold, and `live_edges`
  goes, since both exist only to bound what the pool could not.
- Mechanism, a leaning and not decided: keep the cutoff each track enforces on
  its own writes, and have a writer whose own cold groups cannot pay its debt
  offer it to a few randomly sampled tracks, which is what reaches a quiet one
  (rejected: priority-and-age buckets drained by a maintenance task, the shape
  [moq-dev/moq#2526](https://github.com/moq-dev/moq/pull/2526) removed for its
  process-wide lock on every write). The maintainer doubts idle tracks matter:
  a sparse track settles its own debt whenever it writes, so pinning bytes
  costs writing them. A latest group that joins the evictable population needs
  something to reach it, or its stale access time drags the mean back and
  stalls eviction everywhere, which is why #2526 kept it out of the mean.

Research for it (2026-11-22). Policies that learn recency (SIEVE, S3-FIFO,
W-TinyLFU) keep an ordered queue. The designs with no global ordered structure
are sampled eviction (Redis, hyperbolic caching) and one cutoff each owner
enforces locally (the Dynamic Threshold of shared switch buffers, MGLRU
generations, memcached's tail-age balancing). Pulsar's broker cache trimmed
per ledger, which let the globally oldest entries survive, and
[PIP-430](https://github.com/apache/pulsar/blob/master/pip/pip-430.md)
replaced it.

Public API: likely a relay cache setting if per-tenant shares need a knob.
Wire: none.

## Required

- [Unbounded groups](/quest/m3/unbounded-groups/README.md) - each group copy holds a 32 MB tail and front eviction can shed a live latest group

## Related

- [Broadcast fairness](/quest/m5/broadcast-fairness.md) - the same tenant fair share on cluster links
