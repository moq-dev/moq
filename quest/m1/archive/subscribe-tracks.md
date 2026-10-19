# [L] SUBSCRIBE_TRACKS

## Goal

moq-net speaks moq-transport SUBSCRIBE_TRACKS (draft-18 on) in both
directions through the [TRACKS](/quest/m1/archive/tracks.md) model hook,
instead of refusing it with NOT_SUPPORTED:

- **Inbound.** A moq-transport subscriber's SUBSCRIBE_TRACKS lists every
  track of every broadcast under its namespace prefix, including later tracks
  and broadcasts, but never pushes their data unasked. The peer resumes or
  SUBSCRIBEs what it wants, served from the cache like any SUBSCRIBE.
- **Outbound.** A lite TRACKS whose broadcast is served by a moq-transport
  peer learns that peer's tracks by sending SUBSCRIBE_TRACKS with FORWARD=0
  and reading the PUBLISH each track produces, without its objects. A listed
  track's data comes by resuming that same subscription when a reader wants
  it.

An unsolicited inbound PUBLISH stays refused: moq-net never merges a pushed
track into a pulled copy. A FORWARD=0 PUBLISH answering our own
SUBSCRIBE_TRACKS carries data only once we resume it with the filter our own
SUBSCRIBE would send, so it is a pull, not that merge.

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

| TRACKS event | Inbound: we answer SUBSCRIBE_TRACKS by its FORWARD | Outbound: we send it with FORWARD=0 |
|---|---|---|
| Track added, with TRACK_INFO | FORWARD=0: a paused PUBLISH, its Track Properties carrying TRACK_INFO as SUBSCRIBE_OK does today, or a PUBLISH_SKIPPED (PUBLISH_BLOCKED on draft-18) past the subscription cap. FORWARD=1 or omitted: a PUBLISH_SKIPPED (PUBLISH_BLOCKED on draft-18) naming the track | a PUBLISH, TRACK_INFO from its Track Properties; or a PUBLISH_SKIPPED (PUBLISH_BLOCKED on draft-18), TRACK_INFO from the SUBSCRIBE_OK of a paused SUBSCRIBE we open for it |
| Caught up | nothing to send: moq-transport has no marker | not needed: the lite side sends CAUGHT_UP for this hop at once, and PUBLISHes arrive as live adds |
| Track removed | PUBLISH_DONE with TRACK_ENDED on that PUBLISH; nothing for a skipped track, whose removal reaches the peer only through its own SUBSCRIBE ending with TRACK_ENDED | PUBLISH_DONE with TRACK_ENDED on the held paused PUBLISH or SUBSCRIBE; any other status fails the listing |

- **Inbound answers by the peer's FORWARD**, decided by the maintainer on
  2026-10-19. We never push every track's data unasked.
  - FORWARD=0: a paused PUBLISH per track, which carries Track Properties and
    a TRACK_ENDED removal, and PUBLISH_SKIPPED (PUBLISH_BLOCKED on draft-18)
    past the subscription cap. The peer resumes the PUBLISH to get data, and
    we serve the update's filter from the cache as we would a SUBSCRIBE's. On
    draft-18 its SUBSCRIBE to a track we PUBLISHed fails with
    DUPLICATE_SUBSCRIPTION, as section 5.1 requires.
  - FORWARD=1, or omitted (it defaults to 1): no draft lets the publisher
    downgrade it to a paused PUBLISH. Drafts 18 and 19 say the resulting
    PUBLISHes "will set the FORWARD parameter to 1"; drafts 20 to 22 use the
    SUBSCRIBE_TRACKS parameters as each PUBLISH's initial subscription
    parameters, and forbid PUBLISH_STATE_NOTIFY from changing a
    subscriber-controlled one unasked. So answer each listed track with
    PUBLISH_SKIPPED (PUBLISH_BLOCKED on draft-18), and the peer SUBSCRIBEs
    what it wants. A skip holds no subscription and has no PUBLISH_DONE, so
    a removal reaches the peer only through its own SUBSCRIBE. Draft-18 words
    PUBLISH_BLOCKED for a publisher out of bidirectional streams only;
    draft-19 on allow "any other reason".
  - Rejected, an agent pick: refusing a FORWARD=1 request with REQUEST_ERROR.
    It is legal, but lists nothing, and since FORWARD defaults to 1 it would
    refuse every peer that omits the parameter.
- **Caught up has no IETF equivalent, and needs none.** Caught up means
  caught up with this hop (see TRACKS), so a relay answers a lite TRACKS at
  once with what it holds, whatever its upstream speaks, and every PUBLISH the
  FORWARD=0 SUBSCRIBE_TRACKS brings is a live add. Inbound, nothing stands in
  for it.
- **One subscription per track: the held PUBLISH carries the data.**
  PUBLISH_DONE is the only removal signal, so an outbound PUBLISH is accepted
  paused (PUBLISH_OK, FORWARD=0) and held as the listing entry. It is also
  the track's upstream copy: when a reader wants the data, REQUEST_UPDATE it
  to FORWARD=1 with the filter our SUBSCRIBE would have sent
  (SUBSCRIPTION_FILTER through draft-19, LOCATION_FILTER and FILL_PARAMETERS
  from draft-20, whose section 3.6.2 names this join), and back to FORWARD=0
  when the last reader leaves, rather than unsubscribing. A second SUBSCRIBE
  is not an option on every draft: draft-18 section 5.1 allows one
  subscription per track per role and fails a second with
  DUPLICATE_SUBSCRIPTION; draft-19 on allow several. Nothing is lost: the
  upstream copy is one subscription carrying the union of reader ranges
  either way, and readers keep their own lite subscriptions downstream. The
  costs are that a listed track's upstream subscription lives as long as its
  entry, paused while unread, and that the existing SUBSCRIBE path adopts a
  held PUBLISH as the track's copy instead of opening one. On draft-18, a
  PUBLISH for a track we already SUBSCRIBEd is that same duplicate, so the
  existing SUBSCRIBE becomes the entry and the PUBLISH is refused with
  DUPLICATE_SUBSCRIPTION; for a SUBSCRIBE still pending, end it first, as
  section 5.1 requires. Only TRACK_ENDED ("no longer being published") is a removal,
  and a later PUBLISH for the name is a new add. Any other status
  (GOING_AWAY, EXCESSIVE_LOAD, ...) ends only that subscription while the
  track may still be published, and the peer need not PUBLISH it again, so it
  fails the listing loudly: the lite TRACKS stream resets and the lister
  requests again, which re-issues SUBSCRIBE_TRACKS. Never turn it into a
  removal, which would hide a live track for good; any reader on that copy
  re-subscribes, as after any upstream loss. The cost is a paused
  subscription per listed track at both ends, bounded below.
- **A skipped track is still listed**, decided by the maintainer on
  2026-10-19. A PUBLISH_SKIPPED (PUBLISH_BLOCKED on draft-18) answering our
  FORWARD=0 SUBSCRIBE_TRACKS names a track that exists, so it is an add. It
  carries only the name, with no Track Properties and no stream to end, so
  open a paused SUBSCRIBE (FORWARD=0) for it and hold that as the entry and
  the track's copy, resumed and paused for readers as the held PUBLISH is. Its SUBSCRIBE_OK supplies the
  TRACK_INFO, so the add goes downstream once it arrives, and its
  PUBLISH_DONE with TRACK_ENDED is the removal. A refused paused SUBSCRIBE,
  or one ending with another status, fails the listing loudly, since nothing
  else would ever say the track ended. That SUBSCRIBE needs the same request
  credit the upstream may have run short of when it skipped, so an upstream
  short on capacity fails and retries the listing rather than leave an entry
  that is never removed.
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

- Inbound, a hostile subscriber's FORWARD=0 SUBSCRIBE_TRACKS makes us open a
  paused PUBLISH per track. Each holds a subscription slot; past the cap, tell
  the peer rather than hold more or omit the track silently: PUBLISH_BLOCKED on
  draft-18, PUBLISH_SKIPPED (its rename) from draft-19 on. Draft-17 has no
  SUBSCRIBE_TRACKS (its PUBLISH half rode SUBSCRIBE_NAMESPACE, which stays
  refused), so nothing changes there. A FORWARD=1 listing holds no
  subscription per track, only the listing reader's state, as on lite.
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

Test on mock time through a relay: a moq-transport subscriber's FORWARD=0
SUBSCRIBE_TRACKS receives a paused PUBLISH per track, including a later track
and a later broadcast under the prefix, with no objects until it resumes one,
and a PUBLISH_SKIPPED past the subscription cap; one with FORWARD=1 or none
receives a PUBLISH_SKIPPED per track and no PUBLISH, and its SUBSCRIBE to a
listed track gets data; both on draft-18 (PUBLISH_BLOCKED) and draft-19 on; a lite TRACKS over a moq-transport upstream
lists its tracks with FORWARD=0, a lite SUBSCRIBE resumes the held PUBLISH
with its join filter and gets a new track's first group with no second
upstream SUBSCRIBE (on draft-18 and draft-19 on), the last reader leaving
pauses it and keeps the entry, a PUBLISH_DONE with TRACK_ENDED removes the entry, and one with
another status resets the listing; a PUBLISH_SKIPPED (PUBLISH_BLOCKED on
draft-18) lists the track once its paused SUBSCRIBE answers, resuming it
gets the data, a TRACK_ENDED on that paused SUBSCRIBE removes it,
and a refused paused SUBSCRIBE resets the listing; on draft-18, a PUBLISH for
a track we already SUBSCRIBEd lists it through that SUBSCRIBE and is refused
with DUPLICATE_SUBSCRIPTION; a moq-transport subscriber resuming our paused
PUBLISH gets the groups its filter asks for from the cache; a moq-transport subscriber gets
PUBLISH_DONE when a lite track is removed; an unsolicited PUBLISH is refused.

Public API: none beyond the TRACKS hook. Wire: moq-transport support for an
existing message, a FORWARD=0 PUBLISH held as a listing entry and resumed
for data, and an
inbound listing that answers FORWARD=1 with skips rather than data.

## Required

- [TRACKS](/quest/m1/archive/tracks.md) - the model hook this maps onto
