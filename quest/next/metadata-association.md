# [M] Plan independent timed-metadata association

## Goal

ID3, SCTE-35, emsg, and FLV script tags share a clear timing, grouping, and
placement contract without waiting for experimental SEI separation. Preserve
raw payloads and format-specific information needed by exporters.

## Plan

These formats already arrive outside the video bitstream. Their carriage is
independent of deciding whether removing SEI or AV1 metadata from video is
worthwhile. Each format retains its own catalog section and payload semantics.
Use the broadcast's continuous PTS-to-wall clock for event timing.

Publish events as soon as they are received, using independent metadata group
sequences. Do not wait for a future media GOP or require a media rendition to
exist. Carry the event's presentation time on the continuous broadcast clock,
with optional rendition association when meaningful. This must work for
advance ad cues, sparse metadata, multiple renditions, audio-only sources,
and metadata-only sources.

Delivery time and event time are different values. Specify the outer frame
timestamp and event presentation time while finalizing the shared envelope.
Generic MoQ tracks can carry reordered event timestamps; the forward-only
media-container rule does not automatically apply to them. Test retention
and expiry for a future cue followed by a current event, since relay expiry
uses group timestamps. Preserve source payload and placement independently
of arrival order. Any new exported envelope shape needs maintainer agreement
before dependent implementation.

Define exact placement when an exporter needs it: before the first media,
prefix/suffix or container position where applicable, and ordering of records
with equal timestamps. Keep source placement separate from event time. State
what can be reconstructed exactly and what is only semantically equivalent.

Choose missing-data and completion semantics needed by these consumers rather
than inheriting per-video-frame empty SEI records. If export uses a deadline,
share its existing mux budget and report unavailable metadata; a deadline alone
cannot prove the source contained no metadata. Avoid indefinite waiting on a
sparse track.

Record the chosen Rust/JS catalog and frame contract, update the dependent
quests, and define shared CI fixtures before their implementations start.
This planning quest owns no container parser or SEI stripping implementation.
Any eventual wire-format change belongs in the matching Hang draft.

## Related

- [SEI evaluation](/quest/future/sei/README.md) - a separate decision about extracting codec metadata
