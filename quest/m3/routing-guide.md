# [M] Explain routing and federation

## Goal

A routing concept page and practical setup guidance explain CDN tiers and
outbound local relays, including priorities, costs, and continuity.

## Plan

Moved from m1 to m3 2026-11-13 (maintainer, /quest-plan): it waits on m2
work, and no quest requires one in a later milestone, since a blocker ranks
by its own milestone. ✅ m3 (rejected: m2, where the cut would wait on it;
pulling the m2 blocker into m1). It ranks right after m2, when it can start.

The 2026-10-15 release audit kept federation guidance in m1, until the move
above. Every feature PR still fixes docs it makes stale.

Walk an outbound-only local relay through scoped admission, two-way delivery,
two uplinks, and partition/healing. Explain application-owned connection
selection and priority, and the rank lite-07 routes and claims take as
shipped ([moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987), the
draft's Routing and Resolution): drain, then priority, then cost, an
unmarked route ahead of a marked one at equal rank, and the Rendezvous Hash
that spreads equal claims.

Distinguish a relay's Node ID, a broadcast's Instance and Epoch, a claim
shown unavailable or lost, and a route. A subscription moves between claims
of one Instance; another Instance is a restart. Explain `upstream` (marked)
links, the chain a claim carries across a scoped grant, and that a
non-forwarding endpoint's claims belong to its relay. Keep economic domain
aggregation and measured link-cost policy outside the basic setup.

Use generic public examples and validate CLI/config examples against the
implemented help. Cross-link `doc/bin/relay/cluster.md` and identity guidance
without duplicating reference material. Route health adds its section in m3.

Public API/wire: documentation only, reflecting completed implementations.

## Required

- [Outbound federation](/quest/m3/cluster-routing/federation.md) - tested operator configuration
