# [M] Explain routing and federation

## Goal

A routing concept page and practical setup guidance explain CDN tiers and
outbound local relays, including priorities, costs, and continuity.

## Plan

The 2026-10-15 release audit keeps federation guidance in m1. Deferred P2P
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
without duplicating reference material. Route health adds its section in m2.

Public API/wire: documentation only, reflecting completed implementations.

## Required

- [Outbound federation](/quest/m1/cluster-routing/federation.md) - tested operator configuration
