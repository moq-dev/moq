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
[unbounded groups](/quest/m1/unbounded-groups/README.md), which makes the pool
the only memory bound and lets front eviction shed a live latest group, so
the measurement runs against the final eviction rules. Show with a test
whether a heavy tenant (many sessions and tracks, or a stopped publisher)
evicts a light tenant's recently read groups or holds more than an even share
under a set capacity. If it does, bring options with a recommendation, such
as per-tenant accounts in the pool with debt charged to the tenant over its
share, before building.

Open, for the design: what keys a tenant. Candidates are the grant root,
which exists today, and the issuer-qualified subject from
[grant subject](/quest/m1/cluster-routing/grant-subject.md). Pick the same key
as [broadcast fairness](/quest/m2/broadcast-fairness.md)'s tenant keying.

Public API: likely a relay cache setting if per-tenant shares need a knob.
Wire: none.

## Required

- [Unbounded groups](/quest/m1/unbounded-groups/README.md) - the pool alone bounds memory and front eviction can shed a live latest group

## Related

- [Broadcast fairness](/quest/m2/broadcast-fairness.md) - the same tenant fair share on cluster links
- [Receive pool](/quest/m2/receive-pool.md) - one per-worker budget covering receive credit and the cache
- [Grant subject](/quest/m1/cluster-routing/grant-subject.md) - a candidate tenant key
