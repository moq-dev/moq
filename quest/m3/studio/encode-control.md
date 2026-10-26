# [L] Live encode control

## Goal

`moq-studio` adapts its encode live without a restart: the program and each
source publish simulcast rungs that follow the relay's bandwidth grant, a rung
nobody watches stops encoding, and bitrate and resolution change mid-stream
through catalog updates. OBS's multitrack output does
part of this only on Windows with specific GPUs; this works on every
platform.

## Plan

Today the studio publishes one H.264 rendition at fixed settings and never
reads `bandwidth::Allocator`. Reuse what exists rather than adding a studio
path: the grant follower in `moq_video::encode` and `moq_mux::rate`, the
catalog's in-place updates and new renditions (`doc/concept/hang.md`). The
encode producer already cuts a keyframe when a subscriber joins
(`encode/cuts.rs`), so each rung inherits that and can run a long GOP. Show
each rung's grant, bitrate, and viewers in the Output panel.

## Related

- [Simulcast rung disable](/quest/m1/simulcast-rung-disable.md) - grant-driven rung disable this builds on
- [moq-audio follows the grant](/quest/m2/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - the audio half
- [Scene editing](/quest/m3/studio/scene-editing.md) - canvas resize through a new rendition
