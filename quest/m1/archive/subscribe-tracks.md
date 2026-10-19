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
track into a pulled copy. A FORWARD=0 PUBLISH answering our own
SUBSCRIBE_TRACKS carries no data, so holding it is not that merge.

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
LARGEST_OBJECT, and no objects flow until the subscriber resumes it.

TRACKS' three events (see its Plan) map as follows:

| TRACKS event | Inbound: we answer SUBSCRIBE_TRACKS | Outbound: we send it with FORWARD=0 |
|---|---|---|
| Track added, with TRACK_INFO | a PUBLISH, its Track Properties carrying TRACK_INFO as SUBSCRIBE_OK does today | a PUBLISH; TRACK_INFO from its Track Properties |
| Caught up | nothing to send: moq-transport has no marker | not needed: the lite side reports this hop's state at once, and PUBLISHes arrive as live adds |
| Track removed | PUBLISH_DONE with TRACK_ENDED on that PUBLISH | PUBLISH_DONE on the paused PUBLISH |

- **Caught up has no IETF equivalent, and needs none.** Caught up means
  caught up with this hop (see TRACKS), so a relay answers a lite TRACKS at
  once with what it holds, whatever its upstream speaks, and every PUBLISH the
  FORWARD=0 SUBSCRIBE_TRACKS brings is a live add. Inbound, nothing stands in
  for it.
- **Removal needs the PUBLISH kept.** PUBLISH_DONE is the only removal
  signal, so an outbound PUBLISH is accepted paused (PUBLISH_OK, FORWARD=0)
  and held as the listing entry, while data flows through a separate
  SUBSCRIBE from the existing path, which joins from the cache like a lite
  one. Multiple subscriptions to one track are allowed. Resuming the paused
  PUBLISH instead starts at the live edge (draft-22 section 9.8), missing the
  first groups. TRACK_ENDED means the track is gone; any other status
  (GOING_AWAY, EXCESSIVE_LOAD) also ends what we know of it, so it removes the
  entry too, and a later PUBLISH for the name is a new add. The cost is a
  paused subscription per listed track at both ends, bounded below.
- **Prefix, not broadcast.** SUBSCRIBE_TRACKS matches every namespace under
  the prefix, so a listing of one broadcast filters to its exact namespace.
  A session may hold only one SUBSCRIBE_TRACKS per overlapping prefix
  (PREFIX_OVERLAP), so lite TRACKS for a namespace and one nested under it
  share one upstream request.
- **Push inventory only.** An IETF relay lists the tracks it has been
  PUBLISHed or already holds, not what an upstream would serve on SUBSCRIBE.
  A track served only on request is unlisted, which matches TRACKS' rule.
- **Properties.** A property TRACK_INFO needs but PUBLISH lacks takes the
  same default it takes from SUBSCRIBE_OK today.
- **A non-conforming peer** may send objects despite FORWARD=0; stop reading
  them rather than buffer.

Untrusted input:

- Inbound, a hostile subscriber's SUBSCRIBE_TRACKS makes us open a PUBLISH
  per track. Each holds a subscription slot; past the cap, send
  PUBLISH_SKIPPED (draft-20 on) rather than hold more. Drafts 18 and 19 have
  no skip signal, so there the track is simply not published.
- Outbound, a hostile publisher can PUBLISH without bound. Each held paused
  PUBLISH counts against the session's `announces` cap, as a lite listed track
  does; past it, refuse the PUBLISH (EXCESSIVE_LOAD) rather than hold more. The
  stream and request-ID credit we grant bounds what is in flight.
- Either way, PUBLISH and PUBLISH_DONE churn costs a stream per add and holds
  only the live set, as on lite.

Guidance:

- TRACK_PROPERTY_FILTER and any MUST that adds cost without a behavioral
  benefit may be refused or skipped; say which in the PR.
- Cover drafts 18 and later, plus `just test interop --all` and the IETF
  passthrough suite where it exercises these messages. Update
  `doc/concept/standard.md`, which lists SUBSCRIBE_TRACKS as refused.

Test on mock time through a relay: a moq-transport subscriber's
SUBSCRIBE_TRACKS receives a PUBLISH per track, including a later track and a
later broadcast under the prefix; a lite TRACKS over a moq-transport upstream
lists its tracks with FORWARD=0, its SUBSCRIBE gets a new track's first
group, and a PUBLISH_DONE removes the entry; a moq-transport subscriber gets
PUBLISH_DONE when a lite track is removed; an unsolicited PUBLISH is refused.

Public API: none beyond the TRACKS hook. Wire: moq-transport support for an
existing message, and a FORWARD=0 PUBLISH held paused as a listing entry.

## Required

- [TRACKS](/quest/m1/archive/tracks.md) - the model hook this maps onto
