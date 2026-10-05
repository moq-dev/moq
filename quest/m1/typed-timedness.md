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

Look out for:

- Where an untimed track's frames still need ordering or "has a frame"
  answers (group start, expiry, the live edge), without a timestamp to read.
- What a receiver does with timing that arrives on a track it accepted as
  untimed, such as an IETF object-scope TIMESCALE and Timestamp (imquic
  publishes this way). Refusing would break peers that play today.
- lite-05 and lite-06 always send a Timescale, so an untimed track goes out
  there as timed, with the encoder's send time, as
  [Untimed model](/quest/m1/untimed-model.md) decided.
- Callers built on `Timed`: [Publishing never invents a
  timestamp](/quest/m1/publish-timestamp.md), its JS mirror, and the data
  consumer quests.

Public API: breaking (`track::Info.timescale`, `Timed`). Wire: none until
the untimed quests use it.
