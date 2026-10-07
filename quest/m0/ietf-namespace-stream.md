# [S] SUBSCRIBE_NAMESPACE streams carry NAMESPACE

## Goal

On draft-16 and later, every SUBSCRIBE_NAMESPACE response stream carries
NAMESPACE and NAMESPACE_DONE for each matching namespace, whatever the
peer's SETUP options, in `rs/moq-net` and `js/net`. Today a peer that did not
send our SOLICIT option (0x40B5A), which is every other implementation, gets
an empty stream and only the unsolicited PUBLISH_NAMESPACE pushes.

## Plan

Triaged from Fastly's moq-relay-interop report (2026-09-23 run) on
2026-10-07. d18 §6.2 says the publisher "MUST send a NAMESPACE message to
any subscriber that has sent SUBSCRIBE_NAMESPACE" for a matching prefix,
and §10.18 puts them on the response stream. d16 §9.25 says the same
without the MUST. Both drafts let a publisher send PUBLISH_NAMESPACE to any
subscriber, and neither says anything about duplicates.

Decided 2026-10-07: always fill the stream on d16+, and keep the unsolicited
pushes to non-SOLICIT peers, so those peers hear each namespace twice.
Rejected: stopping unsolicited pushes on d16+ (a peer that never subscribes
would learn nothing), and filling only on d18+ (two behaviours for one
message).

Where it lives: `rs/moq-net/src/ietf/publisher.rs` (around line 2180)
picks `origin.empty()`, or only the hidden remainder, when
`declared.solicit` is unset. Its comment explains the old reason: a peer
hearing both would hold two sources for one namespace. A NAMESPACE is
discovery only, not a route, so that no longer applies; rewrite the comment.
Drafts 14 and 15 keep answering with PUBLISH_NAMESPACE requests, unchanged.
`js/net` mirrors the solicit logic (`js/net/src/ietf/connection.ts`,
`publisher.ts`).

Update `doc/concept/standard.md` (around lines 100-106 and 141-144), which
documents the empty stream.

Test: a non-SOLICIT d16 and d18 peer's SUBSCRIBE_NAMESPACE receives
NAMESPACE for an existing match and for one announced later, then
NAMESPACE_DONE when it ends.

Public API: none. Wire: moq-transport replies move closer to the drafts.
