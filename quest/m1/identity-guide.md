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
as a bug. Show lifetime pins for readers without announcement following and
Start/End/Restart for those that follow siblings. Explain optional expected
epochs, known mismatch refusal, and the weaker legacy best-effort guarantee.

Include lazy epochless claims, exact per-job announcements under fresh
epochs ([claim answer epochs](https://github.com/moq-dev/moq/pull/5678)), and
the cold-start Restart fallback.

Decided 2026-10-27 (maintainer): the guide documents what exists (epochs,
Restart, claim answers, and the cold-start fallback) and no longer waits on
[announcement takeover](/quest/m1/announce-takeover.md); the takeover PR adds
its own section, distinguishing lineage from cross-worker stitching and
showing what happens when either announcement ends. Cover
SUBSCRIBE and FETCH, publisher max-age versus reader max-delay, unordered
arrivals and duplicates, and terminal DROP behavior by supported version.
Do not claim unimplemented behavior: identify current capabilities and link
pending work rather than turning the guide into a release gate for every
future extension. In particular, End+Start cannot invalidate arbitrary old
IETF caches, and our moq-transport publishers ignore FILL_TIMEOUT.

Use a small decision table and one lazy-job lifecycle example. Verify examples
against the implemented APIs and link protocol-specific wire details instead
of duplicating them. Use the existing documentation build checks.

Public API and wire: none; this is a user guide for existing feature owners.

## Related

- [Announcement takeover](/quest/m1/announce-takeover.md) - adds its own section once it lands
