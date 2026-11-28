# [L] First-party encoders default to the cts container

## Goal

First-party importers and encoders that can produce reordered (B-frame) video
publish it in the `cts` container by default, so a track's transport timestamps
are decode times and each frame's presentation time is its transport timestamp
plus the `cts` offset, rather than the `legacy` prefix.

Group splitting, splices, open GOPs, leading pictures, and joins keep their
current behavior on a `cts` track, and an MPEG-TS, FLV, or fMP4 import with
B-frames round-trips through `cts` to every exporter (fMP4, TS, FLV, MKV, HLS)
with the same presentation times as through `legacy`.

## Plan

The timestamp work
([moq-dev/moq#9794](https://github.com/moq-dev/moq/pull/9794)) made a group's
timestamps never decrease and landed the pieces this builds on:

- `moq_mux::container::Frame::decode` and `Container::decode_order`: a
  decode-order producer groups by decode time and writes it to the transport.
  `cts` still only decodes (`cts::Wire::writable` refuses it). A `cts` writer
  with a round-trip test was built and then dropped before merge, until this
  quest's questions settle; restore it from `rs/moq-mux/src/container/cts.rs`
  at moq-dev/moq@a5ae9f23a7327a47b97b167faa5541dae3d6995d.
- `legacy` and `loc` stamp the transport with the newest presentation time so
  far in the group, keeping each frame's own in the payload, so B-frame imports
  keep working without `cts`.
- FLV and fMP4 importers fill `decode` (fMP4 only for a decode-order output);
  MPEG-TS does not yet. All three still default to `legacy`.

Switching the defaults there failed about 40 moq-mux tests whose splice,
open-GOP, and leading-picture handling is keyed by presentation time (for
example `video_continuing_keeps_a_leading_picture_below_its_start`,
`h264_join_drops_a_leading_picture_before_the_old_content_ends`,
`open_gop_spans_hold_no_other_start`, and the TS recovery tests). Rework the
producer's decode-order grouping until those behaviors hold on a `cts` track,
fill `decode` in the MPEG-TS importer, restore the `cts` writer, then flip the
defaults.

Open questions:

- Audio has no reordering: `raw` (no prefix) or `cts` (a zero offset byte).
- MKV has no DTS; rebuild it from the reorder depth, or keep MKV on `legacy`.
- CMAF passthrough stamps a fragment at its earliest presentation time, so a
  sync-led fragment continuing its group with a leading picture below that
  group's latest is now refused. Anchoring at `tfdt` (first decode time) fixes
  it, but a first fragment decoding before the catalog clock's zero (an edit
  list on a verbatim timebase) then shifts that track's presentation times;
  decide how the timebase keeps decode times non-negative.
- libmoq and moq-ffi frame APIs carry no decode time, so a binding cannot
  publish reordered `cts`: keep `cts` refused there until they do.
- A moq-transport draft-18..22 hop carries dense Object IDs and stamps every
  object at its Group ID, losing the decode time a `cts` offset counts from
  (`legacy` survives, since its payload holds the presentation time). Decide
  whether a `cts` track refuses such a hop or the hop stays unsupported.
- The HLS export refuses `cts` renditions (`rs/moq-hls/src/export/rendition.rs`);
  serve them, placing groups by their transport (decode-time) starts.
