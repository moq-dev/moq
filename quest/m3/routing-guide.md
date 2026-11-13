# [M] Explain routing and federation

## Goal

A routing concept page and practical setup guidance explain CDN tiers and
outbound local relays, including priorities, costs, and continuity.

## Plan

Moved from m1 to m3 2026-11-13 (maintainer, /quest-plan): it requires
[outbound federation](/quest/m2/cluster-routing/federation.md), and a quest requiring one in a later milestone stalls,
since `quest unblocked` ranks a blocker by its own milestone. moq-dev/quest's
`quest check` is planned to reject such links.

- ✅ m3 (rejected: m2, which the [release cut](/quest/m3/release/cut.md)
  requires whole, so the cut would wait on this additive work; pulling the
  blocker into m1, which would drag breaking m2 work with it)

The 2026-10-15 release audit kept federation guidance in m1, until the move
above. Deferred P2P
owns the later application-selected peer setup and extends this concept page.
Every feature PR still fixes docs it makes stale.

Walk an outbound-only local relay through scoped admission, two-way delivery,
two uplinks, and partition/healing. Explain application-owned connection
selection and priority, cost comparison within priority, and deterministic
equal-route selection.

Distinguish routing identity, broadcast epoch, advertisement presence,
forwarding, and reachability. Resume requires matching explicit epochs;
epochless changes restart. Keep economic domain aggregation and measured
link-cost policy outside the basic setup.

Use generic public examples and validate CLI/config examples against the
implemented help. Cross-link `doc/bin/relay/cluster.md` and identity guidance
without duplicating reference material. Route health adds its section in m5.

Public API/wire: documentation only, reflecting completed implementations.

## Required

- [Outbound federation](/quest/m2/cluster-routing/federation.md) - tested operator configuration
