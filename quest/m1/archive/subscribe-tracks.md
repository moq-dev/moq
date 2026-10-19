# [L] SUBSCRIBE_TRACKS

## Goal

moq-net speaks moq-transport SUBSCRIBE_TRACKS (draft-18 on) in both
directions through the [TRACKS](/quest/m1/archive/tracks.md) model hook,
instead of refusing it with NOT_SUPPORTED:

- **Inbound.** A moq-transport subscriber's SUBSCRIBE_TRACKS gets a PUBLISH
  for every track of every broadcast under its namespace prefix, including
  later tracks and broadcasts, each served from the cache like a SUBSCRIBE.
- **Outbound.** A lite TRACKS whose broadcast is served by a moq-transport
  peer learns that peer's tracks by sending SUBSCRIBE_TRACKS with FORWARD=0
  and reading the PUBLISH each track produces, without its objects. Each
  listed track is then SUBSCRIBEd like any other.

An unsolicited inbound PUBLISH stays refused: moq-net never merges a pushed
track into a pulled copy.

## Plan

Decided by the maintainer on 2026-10-19 with TRACKS (see its Plan for the
quotes): "we don't need an extension; we could issue forward=0 to learn about
all tracks from IETF peers". This replaces the 2026-10-17 plan, which mapped
SUBSCRIBE_TRACKS 1:1 onto SUBSCRIBE_ALL and accepted inbound PUBLISH as a
pushed track. Accepting PUBLISH is dropped: it is the push-into-pull merge the
maintainer declined.

FORWARD=0, verified against the draft-18 and draft-22 text (draft-22 sections
3.1.1, 3.6, 9.8 and 9.20.18): each PUBLISH a SUBSCRIBE_TRACKS produces starts its
subscription paused, carrying the full track name, Track Properties, and
LARGEST_OBJECT, and no objects flow until the subscriber resumes it. Gaps to
handle:

- **Each PUBLISH is a subscription.** It holds a bidi stream and a request ID
  at both ends. Recommended: answer each with an error (UNINTERESTED) once
  the track is recorded, then SUBSCRIBE through the existing path, so a
  listed track holds no state upstream beyond the listing and joins from the
  cache like a lite one. Resuming the paused PUBLISH instead starts at the
  live edge (draft-22 section 9.8), missing the first groups. Both are legal:
  multiple subscriptions to one track are allowed, and rejecting a PUBLISH
  leaves the SUBSCRIBE_TRACKS open.
- **Prefix, not broadcast.** SUBSCRIBE_TRACKS matches every namespace under
  the prefix, so a listing of one broadcast filters to its exact namespace.
  A session may hold only one SUBSCRIBE_TRACKS per overlapping prefix
  (PREFIX_OVERLAP), so lite TRACKS for a namespace and one nested under it
  share one upstream request.
- **Push inventory only.** An IETF relay lists the tracks it has been
  PUBLISHed or already holds, not what an upstream would serve on SUBSCRIBE.
  A track served only on request is unlisted, which matches TRACKS' rule.
- **No retraction message.** A track's end arrives as PUBLISH_DONE on its
  paused subscription, which the recommended rejection gives up. A listed
  IETF track is retracted when its own SUBSCRIBE ends, or never; say which in
  the PR.
- **Properties.** TRACK_INFO comes from PUBLISH's Track Properties the way it
  comes from SUBSCRIBE_OK today; a property TRACK_INFO needs but PUBLISH lacks
  takes the same default.
- **A non-conforming peer** may send objects despite FORWARD=0; stop reading
  them rather than buffer.

Untrusted input:

- Inbound, a hostile subscriber's SUBSCRIBE_TRACKS makes us open a PUBLISH
  per track. Each holds a subscription slot; past the cap, send
  PUBLISH_SKIPPED (draft-20 on) rather than hold more. Drafts 18 and 19 have
  no skip signal, so there the track is simply not published.
- Outbound, a hostile publisher can PUBLISH without bound. Each track it
  lists counts against the session's `announces` cap, as on lite, and the
  stream and request-ID credit we grant bounds what is in flight.

Guidance:

- TRACK_PROPERTY_FILTER and any MUST that adds cost without a behavioral
  benefit may be refused or skipped; say which in the PR.
- Cover drafts 18 and later, plus `just test interop --all` and the IETF
  passthrough suite where it exercises these messages. Update
  `doc/concept/standard.md`, which lists SUBSCRIBE_TRACKS as refused.

Test on mock time through a relay: a moq-transport subscriber's
SUBSCRIBE_TRACKS receives a PUBLISH per track, including a later track and a
later broadcast under the prefix; a lite TRACKS over a moq-transport upstream
lists its tracks with FORWARD=0 and its SUBSCRIBE gets a new track's first
group; an unsolicited PUBLISH is refused.

Public API: none beyond the TRACKS hook. Wire: moq-transport support for an
existing message, and PUBLISH answered with an error.

## Required

- [TRACKS](/quest/m1/archive/tracks.md) - the model hook this maps onto
