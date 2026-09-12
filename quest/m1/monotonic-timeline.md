# [L] Timelines only move forward

## Goal

A track's timestamps never fall below the live edge its earlier groups
reached. A publisher that has to reset (a flush, a seek, a source restart)
declares a discontinuity by publishing the next group as a marker (one empty
frame, no decodable payload) and resuming in the group after that, continuing
forward from where it was; it cannot rewind. Inside a group timestamps still
reorder freely, since B-frames present before the frames that precede them in
decode order, and open-GOP leading pictures still qualify because they sit
above the previous group's reach.

The marker group is a real object, so a moq-transport peer sees it and the
live edge moves immediately. An empty group (zero objects) is invisible on
moq-transport and means nothing in hang. A consumer MUST NOT submit the marker
to a decoder.

A delivered hole the consumer cannot prove harmless is a playhead event:
re-apply startup delay and skip. It is not a decoder flush. The next group
already starts on a keyframe with parameter sets. A hole is harmless when the
previous group's end meets the next group's first timestamp within 1 ms, which
keeps DTS-derived ids and the HLS importer's packed epoch bits playing
through. A latency skip is a delivered hole, so it bumps the same playhead
generation.

Consumers stop detecting and re-anchoring on undeclared rewinds. A group that
breaks the forward-only rule is a malformed track, not a timeline event.

This also retires the inferred rewind behind #3533: `moq export ts` stalls
video and primary audio for good at the first content join of a source whose
transport timeline is continuous, because the legacy audio importer's
sub-frame backwards step at a PES resync is read as a rewind and the
exporter's backward fence never opens for the bystander tracks. With no
inferred rewinds there is no fence.

js/publish does not emit the marker yet.

## Plan

### Declaration

Today `Producer::discontinuity()` (`rs/moq-mux/src/container/producer.rs:365`)
cuts the open group and publishes an empty group at `seq+1`. Replace it: cut,
publish group N+1 containing a single empty frame whose timestamp is the
exclusive end of the previous epoch, and let the next `write` open N+2. The
empty frame is an object, so the group exists on moq-transport; hang already
forbids submitting an empty video or audio payload to a decoder
(`drafts/draft-lcurley-moq-hang.md:525-534`). Relax "each group MUST start
with a keyframe" (`:511`) so a group that contains only that marker is legal.
Audio uses the same empty-frame marker (not submitted to the decoder). Data
tracks have no marker: empty is payload, so they skip a sequence with no
object in the hole.

Callers stay: h264/h265/opus/aac/legacy importers, TS import, moq-video idle
capture, moq-audio, moq-boy. js/publish is
[its own quest](/quest/m2/js-publish-discontinuity.md).

If the marker arrives, walk now and bump the playhead generation. If it is
shed, N→N+2 is a sequence hole: wait until `max_age` trips or the track
finishes (`rs/moq-mux/src/container/consumer.rs:368-375`), then bump playhead
generation only when the boundary is not 1 ms-contiguous. An idle resume
whose timestamps jumped still jumps the playhead after a shed marker
(#3291); an encoder restart on a continuous clock looks like a DTS id jump
and does not.

### Playhead, not codec reset

The mux discontinuity counter becomes a playhead generation: startup delay
and skip, not `decoder.flush()` / `decoder.reset()`. Remove those flushes
from `rs/moq-video/src/decode/consumer.rs:85-92` and
`rs/moq-audio/src/decode/consumer.rs:283-302`. Audio still re-applies
pre-skip in the play path. Watch's remaining in-flight-frame question is
[#3056](/quest/m2/3056-watch-video-decoder-captures-the-rewind-generation-at.md).

### Publisher refuse rewind

The track producer refuses a frame whose timestamp is below the live edge
established by the groups before its own, returning an error the way an
oversized frame does, so a rewind never reaches the wire. That is `moq-mux`'s
producer and the `js/hang` container producer (`js/hang/src/container/legacy.ts`
has no cross-group live-edge check today). moqsink already re-anchors forward
on a flush and rejects a rewinding base; check the CLI importers, the capture
publishers, and `js/publish` do the same on a source restart, and re-anchor
rather than refuse where the source is trusted.

The legacy MPEG audio importer in `rs/moq-mux/src/container/ts/import.rs` is
one such trusted source and must re-anchor forward. `LegacyStream::write`
takes the PES PTS for the first frame that begins in a PES and then
extrapolates every following frame by its own duration
(`advance_pts`, `:2436`), carrying the extrapolated value across a PES
boundary as `tail_pts` (`:2447`). When frame sync is lost it abandons the
tail and re-anchors on the PES PTS (`pts = pes_base`, `:2377` and `:2395`).
At a content join on a continuous transport timeline that PES PTS sits a few
milliseconds below the extrapolated high-water mark, which is the backwards
step #3533 measured (one MPEG-1 audio resync at the join, `audio frame sync
lost pid=121 resyncs=2`). Once the producer refuses rewinds that sub-frame
step becomes a refused frame and an aborted import rather than a stall, so
the resync must clamp its re-anchor to the track's live edge (or skip the
frame that would land below it) and only then continue from the PES PTS.
A stale tail after a loop wrap, wrong by hours, still re-anchors on the PES.

### Consumer abort rewind

Delete the rewind boundary and its classification in both languages (`Rewind`
in `rs/moq-mux/src/container/consumer.rs:83` and
`js/hang/src/container/consumer.ts:76`). A group below the live edge aborts
the track as malformed. The playhead generation stays, counting declared
marker groups, unproven delivered holes, and latency skips.

### Draft

`drafts/draft-lcurley-moq-hang.md:515-517` loses the empty-group sentences and
the backward clause. State: a group with no decodable frames is a walk-now
discontinuity; empty groups are permitted and mean nothing; after a
discontinuity the timeline continues forward; a delivered sequence hole is a
playhead event unless the boundary is contiguous within 1 ms. The moq-lite
draft is unchanged; enforcement lives above the relay.

### TS export

`Exporter::rewind(backwards)` (`rs/moq-mux/src/container/ts/export.rs:928-952`)
restarts the program clock on a source discontinuity, clearing `last_pcr`,
`last_psi`, and every `si[*].last_emit`; it is keyed on the source's playhead
generation (`:696`). The `backwards` flag (decided at `:581` and `:1145`, and
what fences peers across a backward boundary at `:948`) disappears with
backward rewinds, so the reset takes no argument and its docs lose the
backward case. `Track::admit` (`:178`) then admits any frame in the current
generation and no track is ever left behind in an old one, which is the #3533
fix: the fence that discarded video and primary audio for 40 minutes no
longer exists. Keep `pcr_discontinuity`, the `discontinuity_indicator` on the
PCR packet, since a PCR jump over 100 ms without it is a TR 101 290 error
whichever direction the jump goes.

### Tests

In both languages: the producer refuses a group below the live edge; the
consumer aborts on one; a group with reordered B-frames is accepted; an
open-GOP group whose leading pictures sit below its keyframe but above the
previous group passes; a marker group followed by a forward jump bumps
playhead generation once and does not flush a decoder; a latency skip does
the same; a non-sequential id jump with a contiguous boundary
(`consumer.nonsequential.test.ts`) does not; a zero-budget idle resume whose
timestamps jumped still jumps the playhead after the marker is shed (#3291).
Rewrite `empty_group_declares_a_discontinuity`,
`latency_skip_preserves_empty_group_discontinuity`,
`discontinuity_publishes_an_empty_group`, and
`discontinuity_moves_the_live_edge_off_stale_content` for the marker group
(one empty frame, not zero objects).

TS import: a legacy audio resync whose PES PTS sits below the extrapolated
high-water mark publishes forward from the live edge and does not abort.

TS export, the #3533 regression in `export_test.rs`: two tracks on a
continuous transport timeline whose content restarts, the audio track alone
stepping back by less than one frame at the join; video and audio keep
emitting across it, and the join costs at most one PCR discontinuity and one
PSI re-emission. `rewind_re_emits_tables_and_resumes_the_clock`
(`export_test.rs:2129`) and `rewind_flags_the_break_once_across_tracks`
(`:2307`) are rewritten for the no-backwards world: the break is a declared
marker group, the clock and tables reset once per program break, and no
track is fenced.

Branch from `dev`, where the container consumers carry the current `Rewind`
state and `is_stale` sheds empty groups.

## Closes

- [#3291](https://github.com/moq-dev/moq/issues/3291) - close this issue when the quest finishes
- [#3533](https://github.com/moq-dev/moq/issues/3533) - close this issue when the quest finishes

## Related

- [js/publish discontinuity](/quest/m2/js-publish-discontinuity.md) - the JS container producer and js/publish emit the same marker
- [#3056](/quest/m2/3056-watch-video-decoder-captures-the-rewind-generation-at.md) - whether watch still resets VideoDecoder to drop in-flight chunks when playhead generation bumps
- [#3115](/quest/m2/3115-moqsink-the-publication-has-no-generation-so-a-flush.md) - moqsink's generation model after EOS, the same publisher
- [Open-GOP leading pictures](/quest/m2/open-gop-leading-pictures.md) - latency skip now bumps playhead generation but does not flush the decoder, so leading pictures still decode
- [TS import stream liveness](/quest/m2/3489-ts-import-stream-liveness.md) - the per-stream access-unit reporting that would have shown the #3533 stall at the importer
