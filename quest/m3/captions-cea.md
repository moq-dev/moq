# [L] CEA-608/708 extraction

## Goal

Captions carried inside video SEI become a real text rendition at import. A
large share of existing broadcast content carries captions only this way, so
without extraction those broadcasts have no captions in MoQ at all.

## Plan

Deferred in the 2026-09-30 audit and moved to m3 in the 2026-10-05 audit: no named consumer for timed metadata.

Parse the `user_data_registered_itu_t_t35` SEI payloads that carry CEA-708
(with 608 compatibility bytes inside) out of H.264 and H.265 access units at
import, and publish the decoded cues as a `text` rendition beside the video.

The fiddly part is that 608/708 is a stateful terminal protocol, not a cue
list. Pop-on, roll-up, and paint-on modes each build the visible caption
differently, and the decoder has to track the display buffer to know when a
cue starts and ends. 708 adds a service layer, so a stream can carry several
services (commonly a primary language and a secondary one), each of which
should become its own rendition with its own `lang`.

Emit cues on the shared media clock, taken from the access unit the SEI rode
in, so the text rendition needs no timeline of its own to stay in sync.

Parse at import from the inline SEI. The separation study kept H.264 and HEVC
SEI in the video access unit, so this parser walks that access unit. There is
no sidecar and no strip step. Leave the SEI in place after the cues are
published, so a decoder that reads captions from the bitstream still can.
