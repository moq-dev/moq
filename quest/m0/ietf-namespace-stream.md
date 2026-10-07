# [S] SUBSCRIBE_NAMESPACE streams carry NAMESPACE

## Goal

On draft-16 and later, a SUBSCRIBE_NAMESPACE that asks for namespaces
gets NAMESPACE and NAMESPACE_DONE on its response stream for each matching
namespace, whatever the peer's SETUP options, in `rs/moq-net` and `js/net`.
Today a peer that did not send our SOLICIT option (0x40B5A), which is every
other implementation, gets an empty stream and only the unsolicited
PUBLISH_NAMESPACE pushes.

## Plan

Triaged from Fastly's moq-relay-interop report (2026-09-23 run) on
2026-10-07. d18 §6.2 says the publisher "MUST send a NAMESPACE message to
any subscriber that has sent SUBSCRIBE_NAMESPACE" for a matching prefix,
and §10.18 puts them on the response stream. d16 §9.25 says the same
without the MUST. Both drafts let a publisher send PUBLISH_NAMESPACE to any
subscriber, and neither says anything about duplicates.

On d16 and d17 the message's Subscribe Options choose what the subscriber
wants: PUBLISH (0x00), NAMESPACE (0x01) or both (0x02) (d16 §9.25, d17
§9.20). d18 moved track requests to SUBSCRIBE_TRACKS, so its
SUBSCRIBE_NAMESPACE always asks for namespaces. Both `rs/moq-net` and
`js/net` decode the field but drop it before dispatch, so the publisher
cannot tell the options apart today.

Decided 2026-10-07: fill the stream on d16+ whenever NAMESPACE is
requested, and keep the unsolicited pushes to non-SOLICIT peers, so those
peers hear each namespace twice. Carry Subscribe Options through dispatch:
0x01 and 0x02 both ask for namespaces, so both get NAMESPACE; 0x00 gets none.
We send no PUBLISH for a namespace subscription, so decide how the track half
is answered. For 0x00 (tracks only), refusing is the recommendation, since it
fails loud. For 0x02, the recommendation is to answer with namespaces only,
since refusing would drop the NAMESPACE it asked for.
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

Update `doc/concept/standard.md` where it describes solicit: a non-SOLICIT
d16+ peer now also gets NAMESPACE on its SUBSCRIBE_NAMESPACE stream, so it
hears each namespace twice.

Test: a non-SOLICIT d16 and d18 peer's SUBSCRIBE_NAMESPACE receives
NAMESPACE for an existing match and for one announced later, then
NAMESPACE_DONE when it ends. On d16, options 0x00, 0x01 and 0x02 each get
the chosen behaviour, in both languages.

Public API: none. Wire: moq-transport replies move closer to the drafts.
