# [L] Every frame of a group presents before every frame of a higher group

## Goal

Groups never go back in time: every frame of a group presents strictly before
every frame of any higher group, so a reader bounds a group that isn't whole
by the lowest timestamp it knows in a higher group. Frames within a group may
still reorder (B-frames, open-GOP leading pictures). Sources that overlap
today, such as a TS content join, publish without ending the broadcast and
without rewriting a timestamp. The moq-lite and hang drafts state the rule,
and the producers and every importer keep it, in Rust and JS.

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
(`a_keyframe_overlapping_the_previous_group_publishes`). The moq-lite draft
only orders frame 0s ("a publisher MUST NOT start a Group's frame 0 at or
before that of any Group with a lower start", in its Frame section), and
`@moq/net` accepted a frame at 250 in group 100 after group 200 opened.

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
  handling the rule makes dead. moq-net's frame 0 check (`check_start`) stays
  as it is.

Decided 2026-11-20 (maintainer, /quest-plan auditing the subscription rules
before the lite-07 cut):

- ✅ The strong form: every frame of a lower group presents before every frame
  of a higher one (rejected: the weak form, every frame before the next
  group's frame 0, which bounds an incomplete group only by a higher group's
  frame 0). Conforming HEVC and H.264 already keep it, since a leading picture
  follows every picture of the previous GOP in output order. Only a splice can
  break it.
- ✅ The moq-lite draft states it as a publisher MUST in lite-07, and the cut
  waits on this quest (moved from m3): the cut publishes the rule, and
  first-party producers break it today.

Recommended for a splice: a frame after the keyframe that would present at or
before the previous group's newest frame, such as a RASL picture, is dropped.
It references pictures from before the splice, so it can't decode there
anyway. Check RADL pictures, which do decode, before settling on dropping
them too.

The hang draft's "a group MAY start at the same timestamp as the previous
group" goes too.

Tests: the producer test above becomes one where the overlapping keyframe
continues the group, and a TS import across a content join, including leading
pictures, publishes groups whose frames all precede every frame of the next
group.

Public API: none expected. Wire: none; the moq-lite and hang drafts' group rule
tightens.

## Related

- [An incomplete group expires by the lowest timestamp of a higher group](/quest/m3/expire-by-higher-min.md) - the expiry bound this rule makes safe
- [An fMP4 group start never rewinds the decode timeline](/quest/m3/fmp4-group-start-tfdt.md) - a keyframe repeating its predecessor's timestamp would now continue the group; check which of its cases remain
