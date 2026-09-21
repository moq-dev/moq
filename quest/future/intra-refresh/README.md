# Intra-refresh GOPs

## Goal

Video encoded with periodic intra refresh has no keyframes. Each frame refreshes
a stripe of the picture, so a decoder that starts at the beginning of a sweep is
clean once the sweep completes, and the bitrate never spikes. This questline
makes such video a first-class hang broadcast at both ends: our encoders can
emit it, streams contributed that way import cleanly, and every viewer tunes in
without a visible glitch.

The motivations, in the order they settle tradeoffs: a flat bitrate at low
latency, so a bandwidth grant holds; faster tune-in, since a short refresh cycle
is a short group and the recovery time overlaps the latency buffer instead of
adding to it; and contribution compatibility with hardware encoders and
broadcast feeds that only do intra refresh.

Decisions the quests share:

- One group per refresh cycle. A group starts at the recovery point, the frame
  that begins a sweep, so a viewer joining at any group boundary is clean after
  exactly one cycle and relay shedding keeps its meaning.
- The catalog carries a `warmup` duration per rendition. Decoded output stamped
  within `warmup` of a group start is not presented after a non-continuous
  join, and a subscriber joins that much further back so the first presented
  frame lands at the latency target. The field is generic: audio gets the same
  one for Opus convergence.
- A viewer never shows a partially refreshed picture: cold tune-in and a
  mid-stream skip both decode everything and present nothing until recovery,
  freezing on the last good frame. A group that opens on a true IDR shows at
  once.
- The shared encode config extends the `Gop` contract settled in main,
  and a cut in refresh mode starts a new sweep, never an IDR.
- H.264 and H.265 only. AV1 and VP9 have no standard gradual refresh signal.
  WebCodecs has no intra-refresh option, so js/publish is consumer-only here.
  Backends without the knob refuse refresh mode; NVENC and V4L2 get it now,
  Media Foundation and MediaCodec are follow-ups.

## Quests

- [Consumer warmup](/quest/future/intra-refresh/consumer-warmup.md) - JS and Rust viewers join `warmup` earlier and withhold display until recovery, except at a true IDR
- [H.264 import](/quest/future/intra-refresh/h264-import.md) - the splitter keeps `recovery_frame_cnt` and import publishes `warmup` from it
- [H.265 import](/quest/future/intra-refresh/h265-import.md) - the splitter reads the recovery-point SEI so an HEVC intra-refresh stream forms groups and publishes `warmup`
- [Encode config](/quest/future/intra-refresh/encode-config.md) - refresh mode extends the settled GOP contract; the producer cuts groups per sweep and publishes `warmup`
- [NVENC refresh](/quest/future/intra-refresh/nvenc-refresh.md) - the NVENC backend encodes refresh mode for H.264 and HEVC
- [V4L2 refresh](/quest/future/intra-refresh/v4l2-refresh.md) - the V4L2 backend encodes refresh mode
- [Bindings](/quest/future/intra-refresh/bindings.md) - ffi, libmoq, and every wrapper expose the `Gop` enum
- [Export sync flags](/quest/future/intra-refresh/export-sync-flags.md) - fmp4, MKV, and HLS stop advertising a refresh group start as a sync sample

## Related

- [Audio warmup](/quest/next/audio-warmup.md) - Opus convergence after a mid-stream join uses the same `warmup` field
- [#2067](/quest/next/2067-test-open-gop-h-264-tune-in-end-to-end-leading-picture.md) - the open-GOP fixture and cold tune-in measurement
- [Open-GOP leading pictures](/quest/next/open-gop-leading-pictures.md) - frames stamped before the group's keyframe are the other tune-in trim
- [Catalog warmup](/quest/next/catalog-warmup.md) - the generic `warmup` field this line reads, kept in next for audio and open-GOP tune-in
