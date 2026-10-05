# [S] A track is all timed or all untimed

## Goal

Prototype moq-net types that make a track all-timed or all-untimed, for
example `track::Timed` and `track::Untimed`, and settle the shape that
[#4822](https://github.com/moq-dev/moq/pull/4822) adapts to.

- `track::Info.timescale` becomes an `Option`, where `None` means untimed: no
  TIMESCALE and no Timestamp on the wire, and no receiver-made local stamps.
- Appending a frame whose timedness doesn't match its track is refused, as
  `Error::TimestampMismatch` already refuses a mismatched timescale.
- `moq_net::Timed` (`rs/moq-net/src/model/timed.rs`) folds into this shape,
  dropping its unused clock parameter `T`. That's a breaking change and gets
  an upgrade note.

Mirror the shape in `@moq/net` and the bindings.

The IETF receive path follows the same rule, and
`drafts/draft-lcurley-moq-timestamp.md` says so:

- On a track accepted without TIMESCALE, an object's own object-scope
  TIMESCALE and Timestamp are ignored, and the track stays untimed.
- On a track that declares TIMESCALE, an object without TIMESTAMP makes the
  track malformed.

## Plan

Decided (2026-10-05, maintainer):

- Timedness is a track property. Rejected: `Option<Timestamp>` on every frame
  and groups that mix timed and untimed frames ("Option is just really
  annoying as an API and revolves into a lot of unwrap()"), and a frame type
  generic over its timestamp (`Frame<T>`).
- Rejected: receiver-made arrival stamps on untimed tracks.
- A receiver learns the property when it accepts the track. Drafts and
  versions that can't declare it up front are untimed: IETF drafts 14-16, a
  standalone FETCH that learns no timescale, and lite-01 to lite-04.
- Mock-up first, then #4822 adapts, so contributors and callers take one
  breaking change instead of two. Folding `Timed`'s `T` in here, rather than
  a separate quest, keeps that to one break too.

Decided (2026-10-05, maintainer, settling the open question on object
stamps):

- Timestamps are all-or-nothing per track on the wire too. On a track
  accepted without TIMESCALE, objects that carry their own object-scope
  TIMESCALE and Timestamp (imquic does) are ignored: the track stays untimed
  and the stamps are dropped, so peers that play today keep playing.
  Rejected: refusing them, and deferring the call to the mock-up.
- On a track that declares TIMESCALE, an object with no TIMESTAMP is
  malformed. The receiver handles it under moq-transport's malformed-track
  rules rather than inventing a time. Rejected: repeating the latest
  timestamp, and falling back to arrival time.
- Object-scope TIMESCALE overrides are no longer applied on any track.
- The draft changes ship in this quest's PR, with the implementation, not in
  a separate draft PR first. In `drafts/draft-lcurley-moq-timestamp.md`,
  three rules change: a publisher stamping every object on a TIMESCALE track
  becomes a requirement rather than a SHOULD, a missing Timestamp is
  malformed instead of falling back to arrival time, and receivers stop
  applying object-scope TIMESCALE overrides. Check the lite draft's per-track
  rule too, and update it if it differs.

Look out for:

- Where an untimed track's frames still need ordering or "has a frame"
  answers (group start, expiry, the live edge), without a timestamp to read.
- Which objects the malformed rule covers. Status-only objects (End of
  Group, End of Track) and an empty LOC end marker carry no media time.
  Before landing, check that our publishers and any interop peer that sends
  TIMESCALE stamp every object the rule covers.
- lite-05 and lite-06 always send a Timescale, so an untimed track goes out
  there as timed, with the encoder's send time, as
  [Untimed model](/quest/m1/untimed-model.md) decided.
- Callers built on `Timed`: [Publishing never invents a
  timestamp](/quest/m1/publish-timestamp.md), its JS mirror, and the data
  consumer quests.

Public API: breaking (`track::Info.timescale`, `Timed`). Wire: no encoding
change. Receive semantics on published IETF drafts change: object-scope
stamps on an untimed track are ignored, and a TIMESCALE track missing a
TIMESTAMP is malformed.
