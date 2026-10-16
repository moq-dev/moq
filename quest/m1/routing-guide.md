# [M] Explain routing and federation

## Goal

A routing concept page and practical setup guidance explain how one model
connects CDN tiers, outbound local relays, and application-selected P2P links,
including priorities, costs, and continuity. Route health (m2) adds its own
section when it lands.

## Plan

Decided 2026-10-13: add the concept page and runnable federation/P2P guidance,
not only reference edits. Every feature PR still fixes docs it makes stale.

Walk an outbound-only local relay through scoped admission, two-way delivery,
two uplinks, partition/healing, and a client retaining its CDN session while
using a preferred peer. Explain application-owned connection selection and
priority, cost comparison within priority, deterministic equal-route selection,
and the absence of browser transit. Do not imply direct always beats CDN.

Distinguish routing identity, broadcast epoch, advertisement presence, forwarding
and reachability. Resume requires matching explicit epochs; epochless changes
restart.

Keep economic domain aggregation and measured link-cost policy outside the
basic setup. Use generic public examples, validate CLI/config examples against
the implemented help. In the P2P setup, list an explicitly selected relay STUN
endpoint (`stun:relay.example.com:3478`) among the application's ICE servers;
link to `/bin/relay/config#stun` for the separate opt-in listener. Cross-link `doc/bin/relay/cluster.md`, JS/native P2P,
and identity guidance without duplicating reference material.

Public API/wire: documentation only, reflecting the completed implementations.

## Required

- [Outbound federation](/quest/m1/cluster-routing/federation.md) - tested operator configuration
- [P2P](/quest/m1/p2p/README.md) - implemented application peer selection and handover
