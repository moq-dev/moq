# [S] MKV import splits laced blocks into frames

## Goal

`moq import mkv` publishes each frame of a laced block as its own hang frame
with its own timestamp, so mkvmerge files (which lace audio by default) play.
A laced block whose per-frame duration cannot be determined is refused with
an error that names lacing.

## Plan

`rs/moq-mux/src/container/mkv/import.rs` passes `raw_frame_data()` straight
through for both SimpleBlock and BlockGroup. webm-iterable documents that this
includes the lace headers, so a laced block becomes one undecodable frame.
Each audio block is also marked a keyframe and then `cut`, so each block is
its own group.

Decided (2026-10-07): split Xiph, EBML, and fixed-size lacing, and time the
frames after the first by the track's `DefaultDuration`. Refuse a laced block
on a track without `DefaultDuration` rather than guess from the codec. Check
whether webm-iterable exposes the lacing type and frame splits before
writing a parser. Test with an mkvmerge-style laced Opus or AAC fixture.

Public API: none. Wire: none.

## Related

- [Export delay](/quest/m1/flv-export-delay.md) - the export side of the same container
