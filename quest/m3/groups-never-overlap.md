# [M] A group's frames present before the next group starts

## Goal

Groups never go back in time: every frame of a group presents before the next
group's start, so a reader can bound a group's end from its own frames.
B-frames and open-GOP leading pictures stay allowed within their own group.
Sources that overlap today, such as a TS content join, publish without ending
the broadcast and without rewriting a timestamp. The hang draft, the producer,
and every importer state and keep the rule.

## Plan

[moq-dev/moq#4543](https://github.com/moq-dev/moq/pull/4543) replaced the
importers' live-edge floor and TS `Reanchor` shift with monotonic group starts.
With nothing shifted, a TS content join
([#3798](https://github.com/moq-dev/moq/issues/3798)) publishes an IDR below
the old content's last frame, so the draft now lets frames present below the
previous group's content (`drafts/draft-lcurley-moq-hang.md`, the malformed-group
rules), and even lets a group start at the previous group's start.
`container::Producer::prepare` opens a group for a keyframe above the open
group's start, even one below its newest frame
(`a_keyframe_overlapping_the_previous_group_publishes`).

That overlap is why
[moq-dev/moq#8874](https://github.com/moq-dev/moq/pull/8874) cannot place an
HLS group early with certainty: when the overlap straddles a grid line, an
edge that saw the earlier group and one that joined at the overlapping group
list one segment differently. #8874 documents that case in
`rs/moq-hls/src/export/grid.rs` and `doc/bin/hls.md`; delete both notes here.

Start by finding every source that overlaps and why: the TS content join,
other importers (fMP4, FLV, MKV, RTMP, SRT, HLS), encoders, and the JS
publisher.

Decided 2026-11-15 (maintainer, /quest-plan):

- ✅ Enforce the rule end to end: draft, producer, importers (rejected:
  investigate only and plan the fix later; end the import at an overlapping
  keyframe, which would stop a TS feed at every content join).
- ✅ A keyframe that doesn't start after the open group's newest frame continues
  that group, as one at or below its start already does (rejected: shift the
  broadcast forward at each join, which brings back the rewriting #4543
  removed and drifts the catalog clock; leave it to the implementer). The
  group at a join then runs about two GOPs: a viewer can't join exactly there,
  and HLS lists one over-long segment through its overrun path.
- ✅ Readers assume the rule and don't detect it, per AGENTS.md (rejected:
  `container::Consumer` refusing an overlapping group). A publisher that
  breaks it hurts only its own broadcast. Delete any reader-side overlap
  handling the rule makes dead.

The draft's "a group MAY start at the same timestamp as the previous group"
goes too: a group's start is after every frame of the group before it.

Tests: the producer test above becomes one where the overlapping keyframe
continues the group, and a TS import across a content join publishes groups
whose frames all precede the next group's start.

Public API: none expected. Wire: none; the draft's group rule tightens.

## Related

- [Media readers take declared ends from the group](/quest/m2/time-addressing/consumers.md) - a group's declared end (lite-07 GROUP_END) is what a reader bounds it by; this rule keeps every frame below the next start

- [An fMP4 group start never rewinds the decode timeline](/quest/m3/fmp4-group-start-tfdt.md) - a keyframe repeating its predecessor's timestamp would now continue the group; check which of its cases remain
