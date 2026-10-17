# Intra-refresh GOPs

## Goal

Video encoded with periodic intra refresh has no keyframes. Each frame refreshes
a stripe of the picture, so a decoder that starts at the beginning of a sweep is
clean once the sweep completes, and the bitrate never spikes. This questline
makes such video a first-class hang broadcast on import and playback: streams
contributed that way import cleanly, and every viewer tunes in without a
visible glitch.

Measured on 2026-10-06: at fixed quality, a 60s keyframe interval saves
10-16% against 2s on the 240p-720p rungs and about 10% at 1080p (x264
veryfast, zerolatency, scene-cut detection off). Almost all of that saving is
already at 10s. A 60s group costs the joiner a full GOP of catch-up decode,
about 4s at 1080p on one desktop thread, and a full GOP of bytes. Encoder
refresh mode does not pay for that. Import and playback stay here because
contributed feeds already use intra refresh.

The motivations, in the order they settle tradeoffs: a flat bitrate at low
latency, so a bandwidth grant holds; faster tune-in, since a short refresh cycle
is a short group and the recovery time overlaps the latency buffer instead of
adding to it; and contribution compatibility with hardware encoders and
broadcast feeds that only do intra refresh.

Decisions the quests share:

The recovery-boundary planning prerequisite checks the duration contract
below before implementation. Its precision and publication ordering must
make these presentation guarantees true even when sweep lengths change.

- One group per refresh cycle. A group starts at the recovery point, the frame
  that begins a sweep, so a viewer joining at any group boundary is clean after
  exactly one cycle and relay shedding keeps its meaning.
- The catalog carries a `warmup` duration per rendition. The transcoder
  decodes enough preceding groups to cover it as history before each group it
  transcodes. A player uses it only to gate and size its second decoder. That decoder is clean because it joins a whole group
  earlier, not because of the value, so an imprecise `warmup` only changes how
  long stripes last. The field is generic: audio gets the same one for Opus convergence.
- A player stays live, in the browser and in Rust: a min-delay decoder may
  show stripes after a cold join or skip, and a second decoder with one more
  sweep of delay takes over once its timestamp catches up. The transcoder
  never encodes a partially refreshed picture: every fetched group and every
  group after a skip is preceded by decode-only history. A group that opens on
  a true IDR needs none.
- H.264 and H.265 only. AV1 and VP9 have no standard gradual refresh signal.
  WebCodecs has no intra-refresh option, so js/publish is consumer-only.

## Required

- [Dual decoder](/quest/m2/intra-refresh/dual-decoder.md) - js/watch runs a min-delay and a max-delay decoder and shows the max-delay picture whenever it has caught up
- [Native dual decoder](/quest/m2/intra-refresh/native-dual-decoder.md) - moq-ffi and moq-c players get the same two-decoder live playback through `decode::Consumer`
- [Play dual decoder](/quest/m2/intra-refresh/play-dual-decoder.md) - `moq play`'s paced pipeline reuses that pair
- [Plan: recovery boundaries](/quest/m2/intra-refresh/plan-boundary.md) - settle safe precision and publication ordering before consumers rely on a duration
- [Consumer warmup](/quest/m2/intra-refresh/consumer-warmup.md) - the transcoder decodes the groups covering `warmup` as history before each group, except at a true IDR
- [H.264 import](/quest/m2/intra-refresh/h264-import.md) - the splitter keeps `recovery_frame_cnt` and import publishes `warmup` from it
- [H.265 import](/quest/m2/intra-refresh/h265-import.md) - the splitter reads the recovery-point SEI so an HEVC intra-refresh stream forms groups and publishes `warmup`
- [Export sync flags](/quest/m2/intra-refresh/export-sync-flags.md) - fmp4, MKV, and HLS stop advertising a refresh group start as a sync sample
