# [L] moq-net carries untimed frames faithfully

## Goal

A track published untimed reaches every subscriber untimed, through any
number of relays. A track is all timed or all untimed, decided by whether
`track::Info.timescale` (now an `Option`) is set. Frames and datagrams carry
an `Option<Timestamp>`, and a write whose timedness doesn't match its track is
refused, as `Error::TimestampMismatch` already refuses a mismatched
timescale. No receiver fills in arrival time, and a relay forwards an untimed
track as untimed. IETF tracks
accepted without TIMESCALE, every track on drafts 14-16, a standalone FETCH
that learns no units, and lite-01 to lite-04 tracks arrive untimed. Covers
moq-net, moq-relay, every in-repo caller of the model, and
`drafts/draft-lcurley-moq-timestamp.md`.

Out of scope: publishers that fill in now
([Publishing never invents a timestamp](/quest/m1/publish-timestamp.md)), the
lite-07 encoding of absence ([lite-07 encodes an absent
timestamp](/quest/m1/lite-untimed.md)), and JS, where `@moq/net` already
carries untimed tracks this way.

## Plan

Decided (2026-10-05, maintainer): timedness is per track, not per frame.
The track's property is learned when it is accepted. Drafts and versions
that can't declare it up front are untimed: IETF drafts 14-16, a standalone
FETCH that learns no timescale, and lite-01 to lite-04. Groups that mix timed
and untimed frames, and object-scope timing on an untimed track, are out.
Rejected: receiver-made arrival stamps on untimed tracks.

Decided (2026-10-06, maintainer): the per-track rule is enforced at runtime,
with `Option` types, and lands in
[#4822](https://github.com/moq-dev/moq/pull/4822) itself, so contributors
and callers take one breaking change. A spike typed the publish side
(`track::Info<T>`, `track::Producer<T>`, `group::Producer<T>` over
`Timescale`, `Untimed`, and `Option<Timescale>`) and was rejected:
everything that learns the timeline at runtime (the wire, relays, fetches,
FFI) stayed on the runtime form and needed `erase()`/`timed()` casts, generic
code couldn't call `write_frame`, an uninferred `T` made `write_frame`
ambiguous, and only 4 publisher call sites got cleaner. Rust consumers
outside moq-net rarely read a net frame's timestamp (media time comes from
the payload), so the `Option` costs little. Rejected along with it: typed
frames (`Frame<T>`) and a subscriber typed by SUBSCRIBE_OK. This supersedes
the 2026-10-05 rejection of a per-frame `Option<Timestamp>`. With no mixed
groups, #4822's open questions on mixed groups and the untimed start
snapshot are moot. Notes below that assume mixing are superseded.

Decided (2026-10-01, maintainer): faithful absence on both the publish and
the subscribe side. Today every receiver stamps local arrival time, as the
timestamp draft mandates, and on lite-05 and later a relay forwards those
stamps as if they were real. Any route failover then changes
them, so the timeline jumps, and the "one clock per broadcast" property
breaks. Rejected: first-hop arrival (the failover jump stays), 0 as a
sentinel (collides with a real pts of 0), and `max_delay` on max(wall, pts)
(`max_delay` stays media-time staleness, so a congestion stall can't age
content out, and the pool's wall-clock expiry is the bound). That last
rejection is superseded 2026-10-06 by [One max_age
meaning](/quest/m1/cache-max-age.md), which lands after this quest.

Decided (2026-10-02, planning this split):

- Superseded 2026-10-05: on a track accepted without TIMESCALE, an object's
  own object-scope TIMESCALE and Timestamp are ignored, and the track stays
  untimed. imquic's LOC examples publish this way, so they arrive untimed and
  keep playing. Object-scope units are never applied, on any track.
  Rejected: refusing them.
- On a track that declares TIMESCALE, an object with no Timestamp is
  malformed. The receiver handles it under moq-transport's malformed-track
  rules rather than inventing a time. Rejected: repeating the latest
  timestamp, and falling back to arrival time.
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
  reclaims it. [One max_age meaning](/quest/m1/cache-max-age.md) later gives
  it a wall-clock staleness rule.
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
  (`track.rs`) deliberately stop at the immediate successor and
  leave the bound unknown while it is unstamped, because skipping ahead
  could expire content that is still valid. Preserve that, and check that an
  untimed track neither stalls a cursor nor replays everything.
- `track::Info.timescale` always has a value today. A relay subscribed to an
  IETF track without TIMESCALE currently announces one downstream. It must
  not claim a timeline the source never had, so it becomes an `Option`.
- Model docs that recommend `Timestamp::now` for untimed data
  (`model/{frame,group,track,subscription}.rs`) change with the type.
- In-repo callers follow the type change:
  - moq-mux container consumers and end markers;
  - moq-e2ee;
  - libmoq;
  - moq-ffi's frame and datagram records and track info;
  - the binding wrappers.

  Media consumers that need a time refuse an untimed frame, except for the
  end-marker rule above.
- A FETCH learns track units from SUBSCRIBE_OK (a joining or fill FETCH).
  A standalone FETCH that learns none when it is accepted is untimed (decided
  2026-10-05), until [FETCH_OK properties](/quest/m1/fetch-ok-properties.md)
  or TRACK_STATUS ([Fetch without SUBSCRIBE](/quest/m1/ietf-fetch-only.md))
  carry them. Test that a track accepted with units still yields
  timestamps, alongside the untimed cases.
- Which objects the malformed rule covers. Status-only objects (End of
  Group, End of Track), an empty LOC end marker, and the keep-alives and gap
  markers the draft exempts today carry no media time. Before landing, check
  that our publishers and any interop peer that sends TIMESCALE stamp every
  object the rule covers.

Interop facts (2026-10-02):

- The in-tree interop matrix has no external peers.
- In the community runner:
  - moxygen sends neither TIMESCALE nor Timestamp, so all its objects become
    untimed.
  - imquic uses the object scope, which is ignored, so its objects arrive
    untimed.
  - libquicr sends neither.
  - MOQtail is undetermined.

Draft: the changes to `draft-lcurley-moq-timestamp.md` ship in this PR, with
the implementation. Four per-object rules change: a publisher stamping every
object on a TIMESCALE track becomes a requirement rather than a SHOULD, a
missing Timestamp is malformed instead of falling back to arrival time,
receivers stop applying object-scope TIMESCALE overrides, and a LOC receiver
may no longer read a bare Timestamp on a track without TIMESCALE as
microseconds. Rewrite the remaining arrival-time mandates (no TIMESCALE,
before track properties arrive) as "untimed". An untimed object has no media
time, is never media-stale, starts at the latest group, and is forwarded as
untimed. Check the lite draft's per-track rule too, and update it if it
differs.

Tests: each receive path (lite before lite-05, IETF subgroup, fetch and
datagram) yields an untimed frame, and a relay forwards one untimed.

Public API: breaking (`track::Info.timescale`, frame and datagram
timestamps become `Option`). Wire: no encoding change. Receive semantics on
published drafts change as the timestamp draft says.

## Related

- [Fetch without SUBSCRIBE](/quest/m1/ietf-fetch-only.md) - opting out of properties today falls back to arrival time
- [Publisher timeliness](/quest/m1/qos/publisher-timeliness.md) - relay ingest measures arrival minus timestamp, and must skip untimed frames
- [One max_age meaning](/quest/m1/cache-max-age.md) - gives untimed groups a wall-clock staleness rule, fixes the failover stall this introduces, and replaces the latest-group start
- [Translator](/quest/m1/rs2ts/translator.md) - flags the nested `Option` in first-start resolution this touches
