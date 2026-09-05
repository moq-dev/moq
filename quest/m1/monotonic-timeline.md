# [M] Timelines only move forward

## Goal

A track's timestamps never fall below the live edge its earlier groups
reached. A publisher that has to reset (a flush, a seek, a source restart)
declares a discontinuity by leaving a gap in the group sequence and
continues forward from where it was; it can no longer rewind. Inside a group timestamps still reorder freely,
since B-frames present before the frames that precede them in decode order,
and open-GOP leading pictures still qualify because they sit above the
previous group's reach.

Consumers stop detecting and re-anchoring on undeclared rewinds. A group that
breaks the rule is a malformed track, not a timeline event.

## Plan

Today both container consumers (`Rewind` in
`rs/moq-mux/src/container/consumer.rs` and in
`js/hang/src/container/consumer.ts`) classify a newer group whose timestamps
land before the live edge as a rewind, drop the reneged buffer, and bump the
same counter a declared discontinuity bumps. The hang draft says a declared
discontinuity applies "whether the resumed timestamps move backward or
forward". That machinery is what goes; how a publisher declares one is
[Gap discontinuity](/quest/m1/gap-discontinuity.md).

- **Publisher side, in both container producers.** The track producer refuses
  a frame whose timestamp is below the live edge established by the groups
  before its own, returning an error the way an oversized frame does, so a
  rewind never reaches the wire. That is `moq-mux`'s producer and the
  `js/hang` container producer (`js/hang/src/container/legacy.ts` has no
  cross-group live-edge check today), so a browser publisher cannot emit a
  sequence the updated consumers reject. moqsink already re-anchors forward
  on a flush and rejects a rewinding base; check the CLI importers, the
  capture publishers, and `js/publish` do the same on a source restart, and
  re-anchor rather than refuse where the source is trusted.
- **Consumer side, in hang.** Delete the rewind boundary and its
  classification in both languages. A group below the live edge aborts the
  track as malformed. The discontinuity counter stays, counting declared
  discontinuities only; `js/watch` keeps resetting its decoders on it
  ([#3056](/quest/m1/3056-watch-video-decoder-captures-the-rewind-generation-at.md)).
- **Draft.** `drafts/draft-lcurley-moq-hang.md` loses the backward clause and
  states the rule: after a discontinuity the timeline continues forward. The
  moq-lite draft is unchanged; enforcement lives above the relay, in the
  media layer that knows what a group is.
- **TS export.** [moq#3375](https://github.com/moq-dev/moq/pull/3375) lands
  first and keys its reset on the discontinuity counter, which keeps working
  once the counter only counts declared discontinuities. After it, remove the
  `last_psi` / `last_si` / `last_pcr` reset a forward jump makes redundant and
  reword its docs; keep `discontinuity_indicator` on the PCR packet, since a
  PCR jump over 100 ms without it is a TR 101 290 error whichever direction
  the jump goes.

Tests, in both languages: the producer refuses a group below the live edge;
the consumer aborts on one; a group with reordered B-frames is accepted; an open-GOP group whose
leading pictures sit below its keyframe but above the previous group passes;
a declared discontinuity followed by a forward jump passes and bumps the
counter once; moqsink's flush re-anchor produces a forward timeline.

Branch from `dev`, where the container consumers carry the current `Rewind`
state.

## Required

- [Gap discontinuity](/quest/m1/gap-discontinuity.md) - settles how a publisher declares the discontinuity this rule continues from
- [moq#3375](https://github.com/moq-dev/moq/pull/3375) has merged, so the TS export reset it adds is keyed on the discontinuity counter before this quest trims it

## Related

- [Duration marker](/quest/m2/duration-marker.md) - the empty frame that closes a group, which is not a discontinuity
- [#3056](/quest/m1/3056-watch-video-decoder-captures-the-rewind-generation-at.md) - the watch decoder reset that keeps mattering for declared discontinuities
- [#3115](/quest/m2/3115-moqsink-the-publication-has-no-generation-so-a-flush.md) - moqsink's generation model after EOS, the same publisher
- [#2833](/quest/m0/2833-moq-export-ts-a-rewound-timeline-stalls-the-si-table.md) - the TS export fix this trims once it lands
