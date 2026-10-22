# [L] Outbound SUBSCRIBE_TRACKS

## Goal

A lite TRACKS whose broadcast is served by a moq-transport peer (draft-18 on)
learns that peer's tracks by sending SUBSCRIBE_TRACKS with FORWARD=0 and
reading the PUBLISH each track produces, without its objects. A listed
track's data comes by resuming that same subscription when a reader wants it.
Today the Rust subscriber never sends SUBSCRIBE_TRACKS, so a lite listing over
a moq-transport upstream lists nothing.

An unsolicited inbound PUBLISH stays refused: moq-net never merges a pushed
track into a pulled copy. A FORWARD=0 PUBLISH answering our own
SUBSCRIBE_TRACKS carries data only once we resume it with the filter our own
SUBSCRIBE would send, so it is a pull, not that merge.

## Plan

Split from the inbound half, which answers a peer's SUBSCRIBE_TRACKS through
`broadcast::Consumer::tracks` (`rs/moq-net/src/ietf/publisher/tracks.rs`).
The maintainer's 2026-10-19 decisions, recorded with TRACKS: "we don't need an
extension; we could issue forward=0 to learn about all tracks from IETF peers".

TRACKS' events map as follows:

| TRACKS event | We send SUBSCRIBE_TRACKS with FORWARD=0 |
|---|---|
| Track added, with TRACK_INFO | a PUBLISH, TRACK_INFO from its Track Properties; or a PUBLISH_SKIPPED (PUBLISH_BLOCKED on draft-18), TRACK_INFO from the SUBSCRIBE_OK of a paused SUBSCRIBE we open for it |
| Synced | not needed: the lite side sends SYNCED for this hop at once, and PUBLISHes arrive as live adds |
| Track removed | PUBLISH_DONE with TRACK_ENDED on the held paused PUBLISH or SUBSCRIBE; any other status fails the listing |

- **One subscription per track: the held PUBLISH carries the data.**
  PUBLISH_DONE is the only removal signal, so an outbound PUBLISH is accepted
  paused (PUBLISH_OK, FORWARD=0) and held as the listing entry. It is also
  the track's upstream copy: when a reader wants the data, REQUEST_UPDATE it
  to FORWARD=1 with the filter our SUBSCRIBE would have sent
  (SUBSCRIPTION_FILTER through draft-19, LOCATION_FILTER and FILL_PARAMETERS
  from draft-20, whose section 3.6.2 names this join), and back to FORWARD=0
  when the last reader leaves, rather than unsubscribing. On draft-18 and
  draft-19 a filter does not deliver objects already published, so a start
  below the Largest Object also sends a Joining FETCH on the held
  subscription's request ID, as the existing pre-draft-20 SUBSCRIBE path
  does. Draft-18 section 5.1 allows one subscription per track per role and
  fails a second with DUPLICATE_SUBSCRIPTION; draft-19 on allow several.
  The upstream copy is one subscription carrying the union of reader ranges
  either way, and the existing SUBSCRIBE path adopts a held PUBLISH as the
  track's copy instead of opening one. Conversely, a PUBLISH for a track we
  already SUBSCRIBEd (established or pending) is redundant: the existing
  SUBSCRIBE becomes the entry, and the PUBLISH is refused with
  DUPLICATE_SUBSCRIPTION on draft-18 and UNINTERESTED from draft-19. Only
  TRACK_ENDED is a removal. Any other status fails the listing loudly: the
  lite TRACKS stream resets and the lister requests again, never turning it
  into a removal that would hide a live track for good.
- **A skipped track is still listed.** A PUBLISH_SKIPPED (PUBLISH_BLOCKED on
  draft-18) names a track that exists, so open a paused SUBSCRIBE (FORWARD=0)
  for it and hold that as the entry and the copy. Its SUBSCRIBE_OK supplies
  the TRACK_INFO, so the add goes downstream once it arrives, and its
  PUBLISH_DONE with TRACK_ENDED is the removal. A refused paused SUBSCRIBE,
  or one ending with another status, fails the listing loudly.
- **One SUBSCRIBE_TRACKS per nesting chain, never widened in place.** Two
  prefixes overlap only when one is a prefix of the other, which the peer
  refuses with PREFIX_OVERLAP. Siblings get separate requests. A nested
  listing under a broader request reuses it and filters locally. A broader
  listing cancels the nested requests, keeping the PUBLISH subscriptions they
  made, and opens only once the peer has closed every cancelled stream: close
  our sending half (FIN on draft-18, RESET_STREAM from draft-19), keep
  reading until the peer closes its half, and delay STOP_SENDING. If the
  broader request is refused, every listing it was meant to serve resets
  loudly and re-requests. Rejected: widening in place with REQUEST_UPDATE,
  and one request per session at the common ancestor.
- **Push inventory only.** An IETF relay lists the tracks it has been
  PUBLISHed or already holds. A track served only on request is unlisted,
  which matches TRACKS' rule.
- **Properties.** A property TRACK_INFO needs but PUBLISH lacks takes the
  same default it takes from SUBSCRIBE_OK today.
- **Draft-22 parameters.** Draft-22 section 9.18 drops SUBSCRIBER_PRIORITY,
  LOCATION_FILTER, and NEW_GROUP_REQUEST from SUBSCRIBE_TRACKS, while section
  3.6.2 still allows any subscription parameter. Send only the section 9.18
  set on draft-22 (`ietf::SubscribeTracks` encodes all of them today), and put
  the join filter on the resuming REQUEST_UPDATE. The inbound decoder accepts
  both readings.
- **A non-conforming peer** may send objects despite FORWARD=0; stop reading
  them rather than buffer.

Untrusted input: a hostile publisher can PUBLISH without bound. Each held
paused PUBLISH counts against the session's `announces` cap, as a lite listed
track does; past it, refuse the PUBLISH (EXCESSIVE_LOAD). The stream and
request-ID credit we grant bounds what is in flight.

Test on mock time over drafts 18 and later: a lite TRACKS over a
moq-transport upstream lists its tracks with FORWARD=0; a lite SUBSCRIBE
resumes the held PUBLISH with its join filter and gets a new track's first
group with no second upstream SUBSCRIBE; a past start on draft-18 and
draft-19 gets cached groups through a Joining FETCH; the last reader leaving
pauses it and keeps the entry; TRACK_ENDED removes the entry and another
status resets the listing; a skipped track lists once its paused SUBSCRIBE
answers; TRACKS(`/a/b`) after TRACKS(`/a`) sends nothing; TRACKS(`/a`) after
TRACKS(`/a/b`) and TRACKS(`/a/c`) cancels both and opens only after the peer
closes them; a PUBLISH for a track we already SUBSCRIBEd is refused; an
unsolicited PUBLISH is refused.

Public API: none beyond the TRACKS hook. Wire: moq-transport support for an
existing message.
