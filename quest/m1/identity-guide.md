# [M] Explain broadcast identity and recovery

## Goal

One upstream guide lets publisher, player and relay authors choose identities
and recover without mixing catalog, media or cached content from different
broadcast instances. Downstream applications keep operational details in
their own docs and link to this shared contract.

## Plan

Decided in the 2026-10-10 audit: add a dedicated identity/recovery guide in m1,
replacing the earlier inline-only decision. Existing docs made stale by a
feature still change in its implementation PR. This page consolidates the
implemented contract, with links from the relevant concept and library docs.

Explain path plus epoch as content identity, not an encoder configuration
hash; unique epochs for replacement producers; same-epoch replicas; sticky
handles; explicit Restart and player resubscription; and publisher name reuse
as a bug. Show catalog-instance lifetime pins, optional expected sibling
epochs, known mismatch refusal, and the weaker legacy best-effort guarantee.

Include lazy epochless claims, exact per-job announcements, the accepted
cold-start Restart fallback, and proven Takeover preserving the same instance
with an independent route. Distinguish lineage from arbitrary cross-worker
stitching, and show what happens when either announcement ends. Cover
SUBSCRIBE and FETCH, publisher max-age versus reader max-delay, unordered
arrivals and duplicates, and terminal DROP behavior by supported version.
Do not claim unimplemented behavior: identify current capabilities and link
pending work rather than turning the guide into a release gate for every
future extension. In particular, End+Start cannot invalidate arbitrary old
IETF caches, and FILL_TIMEOUT is a waiting approximation, not exact age control.

Use a small decision table and one lazy-job lifecycle example. Verify examples
against the implemented APIs and link protocol-specific wire details instead
of duplicating them. Use the existing documentation build checks.

Public API and wire: none; this is a user guide for existing feature owners.

## Required

- [Catalog references](/quest/m0/broadcast-epoch/catalog-references.md) - establishes the shared pinning behavior
- [Announcement takeover](/quest/m1/announce-takeover.md) - establishes the continuity operation the guide demonstrates
