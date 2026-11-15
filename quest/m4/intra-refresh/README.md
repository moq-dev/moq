# Intra-refresh GOPs

## Goal

Video encoded with periodic intra refresh has no keyframes. Each frame refreshes
a stripe of the picture, so a decoder that starts at the beginning of a sweep is
clean once the sweep completes, and the bitrate never spikes. This questline
makes such video a first-class hang broadcast on import and playback: streams
contributed that way import cleanly, and viewers stay live while recovering
to a clean picture. A cold join or skip may show refresh stripes until the
second decoder catches up, as the playback decision below permits.

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

Decided 2026-11-15 (maintainer, /quest-complete): ✅ a catalog `warmup` is a
hint nothing enforces (rejected: refusing the stream once a sweep exceeds the
published `warmup`; a bound published before the content it covers; a fixed
upper bound from importer config). Since
[moq-dev/moq#8857](https://github.com/moq-dev/moq/pull/8857) plays a `warmup`
rendition through one decoder, the value only tells a player how early to
join and how long to hide pictures, so a sweep longer than published shows
refresh stripes a little longer: a cosmetic cost, not a correctness one.

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
