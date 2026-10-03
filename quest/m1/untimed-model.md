# [L] moq-net carries untimed frames faithfully

## Goal

A frame or datagram published without a timestamp reaches every subscriber
without one, through any number of relays. moq-net's model carries
`Option<Timestamp>`. No receiver fills in arrival time, and a relay forwards
absence as absence. IETF objects on a track without TIMESCALE or without a
Timestamp property, and lite-01 to lite-04 frames, arrive untimed. Covers
moq-net, moq-relay, every in-repo caller of the model, and
`drafts/draft-lcurley-moq-timestamp.md`.

Out of scope: publishers that fill in now
([Publishing never invents a timestamp](/quest/m1/publish-timestamp.md)), the
lite-07 encoding of absence ([lite-07 encodes an absent
timestamp](/quest/m1/lite-untimed.md)), and JS
([@moq/net carries untimed frames faithfully](/quest/m1/js-untimed-model.md)).

## Plan

Decided (2026-10-01, maintainer): faithful absence on both the publish and
the subscribe side. Today every receiver stamps local arrival time, as the
timestamp draft mandates, and on lite-05 and later a relay forwards those
stamps as if they were real. A failover to another first hop then changes
them, so the timeline jumps, and the "one clock per broadcast" property
breaks. Rejected: first-hop arrival (the failover jump stays), 0 as a
sentinel (collides with a real pts of 0), and `max_age` on max(wall, pts)
(`max_age` stays media-time staleness, so a congestion stall can't age
content out, and the pool's wall-clock expiry is the bound).

Decided (2026-10-02, planning this split):

- An IETF object that carries its own TIMESCALE and Timestamp properties is
  timed, even when the track sent no TIMESCALE. The draft already allows an
  object-scope TIMESCALE. Reading it is faithful, not invented. imquic's LOC
  examples publish this way.
- A legacy or LOC end marker (an empty frame) that arrives untimed is ignored.
  The group then ends without a precise end bound. Its frames still play
  from their payload timestamps, and only the last frame's duration is
  estimated. Refusing would turn streams that play today into failures.
- Until lite-07 encodes absence, a lite encoder writes its send time for an
  untimed frame, on every lite version from lite-05. That's what producers
  effectively do today, so nothing regresses. Document it in the lite draft
  as a downgrade for lite-05 and lite-06.

Untimed semantics, from the decision:

- An untimed group is never media-stale. The pool's wall-clock expiry still
  reclaims it.
- Start resolution on an untimed track picks the latest group rather than
  replaying the cache, as pre-lite-06 sessions already do.

Things to look out for:

- Several model decisions read "has a timestamp" as "has a frame".
  `group.rs`'s `poll_timestamp` is the clearest case, and `GroupExpiry` uses
  it. An untimed frame must still count as a frame there. Key "has
  presented" off frames written (or fin), and keep the group's
  `Option<Timestamp>` for media time only, so an open group and a group of
  untimed frames don't share one `None`.
- The live edge skips unstamped groups. Reach and successor search
  (`track.rs`, `resume.rs`) deliberately stop at the immediate successor and
  leave the bound unknown while it is unstamped, because skipping ahead
  could expire content that is still valid. An untimed successor keeps that
  bound unknown for good, so the timed group before it is kept. Preserve
  that, and check that an untimed track neither stalls a cursor nor replays
  everything. Cover tracks that mix timed and untimed groups.
- `track::Info.timescale` always has a value today. A relay subscribed to an
  IETF track without TIMESCALE currently announces one downstream. It must
  not claim a timeline the source never had.
- Model docs that recommend `Timestamp::now` for untimed data
  (`model/{frame,group,track,subscription}.rs`) change with the type.
- In-repo callers follow the type change:
  - moq-mux container consumers and end markers;
  - moq-e2ee;
  - libmoq;
  - moq-ffi's frame and datagram records, whose `timestamp_us` becomes
    optional;
  - the binding wrappers.

  Media consumers that need a time refuse an untimed frame, except for the
  end-marker rule above.
- Only objects with neither track units nor object-level units are untimed.
  A FETCH learns track units from SUBSCRIBE_OK (a joining or fill FETCH),
  from TRACK_STATUS ([Fetch without SUBSCRIBE](/quest/m1/ietf-fetch-only.md)),
  or from [FETCH_OK properties](/quest/m1/fetch-ok-properties.md), whichever
  applies. Until those last two land, a standalone FETCH keeps timestamps
  only on objects that carry their own units. Test that known units, from
  the track or the object, still yield timestamps, alongside the untimed
  cases.

Interop facts (2026-10-02):

- The in-tree interop matrix has no external peers.
- In the community runner:
  - moxygen sends neither TIMESCALE nor Timestamp, so all its objects become
    untimed.
  - imquic uses the object scope.
  - libquicr sends neither.
  - MOQtail is undetermined.

Draft: rewrite the arrival-time mandates in
`draft-lcurley-moq-timestamp.md` (no TIMESCALE, before track properties
arrive, an object without a Timestamp) as "untimed". An untimed object has no
media time, is never media-stale, starts at the latest group, and is
forwarded as untimed.

Tests: each receive path (lite before lite-05, IETF subgroup, fetch and
datagram) yields an untimed frame, and a relay forwards one untimed.

Public API: breaking (`Frame`, `Datagram` and track info
timestamps become optional). Wire: no encoding change. Receive semantics on
published drafts change as the timestamp draft says.

## Related

- [IETF timestamp units](/quest/m1/ietf-timestamp-units.md) - drafts 14-16 stop sending timestamps without units, which then arrive untimed
- [Fetch without SUBSCRIBE](/quest/m1/ietf-fetch-only.md) - opting out of properties today falls back to arrival time
- [Publisher timeliness](/quest/m1/qos/publisher-timeliness.md) - relay ingest measures arrival minus timestamp, and must skip untimed frames
- [Plan: cache age-out](/quest/m1/cache-wall-eviction.md) - whatever it decides, untimed groups age out only through the pool's expiry
- [Translator](/quest/m1/rs2ts/translator.md) - flags the nested `Option` in first-start resolution this touches
