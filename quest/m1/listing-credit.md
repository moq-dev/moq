# [M] SUBSCRIBE_TRACKS listings follow stream credit

## Goal

A draft-17+ SUBSCRIBE_TRACKS listing holds state in proportion to what the
peer reads, not to the catalog. Each listing PUBLISH opens only as the
peer's stream credit admits it, and a peer that stops reading stops the
enumeration.

## Plan

In `rs/moq-net/src/ietf/publisher/tracks.rs`, every FORWARD=1 or unfittable
track goes into `request.skips` as the listing is enumerated, and
`poll_write_skips` paces only the writes. A peer that stops reading makes the
session hold state proportional to the whole catalog, multiplied across
peers. After [Session limits](/quest/m1/api-audit/session-limits.md), the
listing PUBLISHes are bounded only by `Limits::requests`, a runtime cap.

Decided 2026-10-23 (maintainer): replace the cap with flow control. Couple
enumeration to write backpressure, open each PUBLISH as stream credit admits
it, and drop the listing's use of `Limits::requests`. Test with a peer that
withholds credit and reads nothing.

Public API: none expected. Wire: none.

## Required

- [Session limits](/quest/m1/api-audit/session-limits.md) - rewrites the same listing code
