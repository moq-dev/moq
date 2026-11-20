# [L] lite-07 has no relative anchors, and Min Age places a live join

## Goal

A lite-07 subscription starts at a time, a Position, or no anchor, and ends at
a time or not at all. The Relative anchor leaves the wire, the Rust and
`@moq/net` range API, moq-ffi, libmoq, and every binding. A live join is a
range with no start anchor and a stated Subscriber Min Age (`Delay::min`).
The serving hop starts it at the group containing its live edge minus the Min
Age. A relay copy that can't prove that group from its cache asks upstream and
waits, as a time start does. The join then delivers the same groups from a
relay copy as from the origin.

## Plan

Decided 2026-11-20 (maintainer, /quest-plan of a pre-cut audit of the lite-07
and hang-04 wire changes):

- ✅ Remove relative anchors, as both starts and ends (rejected: relative
  starts only; ends reported in SUBSCRIBE_OK and SUBSCRIBE_STARTED, as
  [moq-dev/moq#9198](https://github.com/moq-dev/moq/pull/9198)'s follow-up
  recommended; ends inferred from accounting). A count of groups back
  resolves against whatever the serving hop holds. A copy holding groups 2
  and 9 of 0..9 counts one back to 2, while the origin counts to 8, and no
  message reports where an end resolved. Only tests used a count above 0;
  production used only `Range::live()`, which is Relative 0.
- ✅ A live join is no start anchor plus Subscriber Min Age (rejected: a
  dedicated Live anchor type). In the maintainer's words, it is "literally
  called min age": the subscriber says how far behind the live edge it plays,
  and that is where it starts.
- ✅ A hop fills to reach a live join's start (rejected: the draft's "starts
  from what it holds and never fetches"). It resolves the start as a time
  start at its live edge minus the Min Age. It answers from its cache only
  what it can prove upstream would answer; otherwise it asks upstream and
  waits. A copy that has been live since a reader with an equal or larger Min
  Age already holds those groups, so a join waits only for a larger Min Age or
  an evicted group.
- Consequence, recorded for review: with no start anchor, an unstated Min Age
  means as far back as Max Age allows (today's `Range::oldest()`), and a
  stated zero means the latest group. A relay forwarding several
  subscriptions carries the largest Min Age, counting "not stated" as the
  largest. An oldest reader merged with a live reader still gets its backlog,
  which the draft's "carry the largest stated value, and not stated when none
  is" would cut off.
- Closed: [moq-dev/moq#9198](https://github.com/moq-dev/moq/pull/9198), whose
  upstream asks resolved relative starts on a copy, and its late-relative-end
  follow-up.

Recommended, for the implementer:

- `Range::live()` and JS `LIVE` go: a live join is the default range with
  `Delay::min` set. A range's end becomes a time type, so a non-time end
  can't be written; today an `Oldest` end is refused at runtime.
- A scale-less track measures Min Age by arrival, which differs per hop.
  Measure it at the hop that answers, and have a copy ask upstream when its
  arrivals don't reach back far enough.
- moq-transport: a live join maps to the filter `Range::live()` maps to
  today. A third-party subscriber's relative joining FETCH or relative start
  filter (`ietf/filter.rs` `Filter::Relative`): serve a count of 0 as a live
  join with a Min Age of zero, and refuse a larger count with NOT_SUPPORTED.
  Counting back by Group ID means nothing on a time-keyed track, and AGENTS.md
  keeps moq-transport compatibility low-effort.

Where it lives:

- Draft `drafts/draft-lcurley-moq-lite.md`: SUBSCRIBE's Anchor Type
  (Relative goes; renumber freely, since lite-07-wip is unpublished), "The
  publisher resolves a Relative or Time Anchor", the live-join paragraph,
  Subscriber Min Age's relay rule, the whole-group rule naming Relative, and
  the changelog. `doc/concept/moq-lite.md` says the same for users.
- Rust: `model/subscription.rs` (`Anchor`, `Range::live`, `Delay`),
  `model/subscription/window.rs` (`end`, `Holdings::back`), `model/track.rs`
  (`TrackState::relative_starts`, `Feed::Live`'s counting), `lite/subscribe.rs`
  (anchor codec), `lite/subscriber/fit.rs` (merging relative ranges).
- JS: `js/net/src/track.ts` (`Anchor`, `Range`), `js/net/src/window.ts`
  (`LIVE`, relative handling), `js/net/src/ranges.ts`, `js/net/src/lite/`.
- Callers of `Range::live()`: moq-audio, moq-cli `play`, moq-mux, moq-transcode,
  moq-video, and libmoq `consume.rs`.
- Bindings: moq-ffi `MoqAnchor::Relative` and libmoq `MOQ_ANCHOR_RELATIVE`,
  with every wrapper together.

Tests: a live join whose Min Age reaches past what a copy holds reads the
same groups from the copy as from the origin, and the copy asks upstream; an
oldest reader and a live reader on one relay each get their own start; an
anchor type the draft no longer assigns is a PROTOCOL_VIOLATION. Run
`just test interop --all`, since the wire and moq-ffi change.

Public API: breaking in Rust, JS, moq-ffi, libmoq, and the bindings.
Wire: lite-07-wip only.

## Related

- [Latest readers join live by their Min Age](/quest/m4/readers-range-live.md) - the readers that floor at the largest group move to this join
- [lite-07 Group Streams carry each time once](/quest/m2/time-addressing/group-times.md) - a time inside a skipped span belongs to no group, which changes where a time start lands
