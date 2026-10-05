# [M] Timelines declare their segment duration

## Goal

Each timeline in the catalog `archive` map declares its own nominal segment
duration, derived from what the application reports (like bitrate or
framerate), or estimated by the publisher when the application doesn't know
it. The broadcast-wide `durationMax`, filled from moq-mux's 10s default, is deleted. Every edge reads
the same declared value, so an HLS edge can fix its target duration from it.

## Plan

Decided in planning (09-29), after the hls-target agent found that nothing in
the catalog declares a usable target:

- **Shape.** Each `timelines` entry becomes an object:
  `timelines: { video: { track: "video.timeline.z", duration: 4000 } }`, in
  the archive `timescale`. The root `durationMax` goes away. This is a break in
  place on this line, with no compatibility path. Update `rs/hang`,
  `drafts/draft-lcurley-moq-hang.md`, and every `durationMax` reference
  (grep it, including `doc/`). The JS port happens in
  [JS per-track timelines](/quest/m1/archive/js-timelines.md).
- **A 4s minimum.** `duration_min` defaults to 4s (from 2s), so an archive
  doesn't write a stream of tiny objects to S3. A record still ends at the
  first group boundary at or past the minimum. This also packs audio, where
  moq-audio publishes each ~20 ms codec packet as its own group (Codex on
  this plan's PR).
- **Meaning: a nominal target, not a hard max.** Records start on keyframes. A
  GOP longer than the declared duration overruns, and moq-hls lists the overrun
  with a warning. A mid-group split remains only as a safety ceiling at a
  multiple of the declared duration (pick one, for example 3x), which is
  internal and not declared. Before an unhinted track declares, the ceiling
  is a fixed default (today's 10s). A hard max that splits at the target was
  rejected: jitter makes slivers, and segments would start without a keyframe.
- **Reported by the application.** moq-mux takes an optional per-track GOP
  hint. moq-video's encoder sets it from its `encode::Gop`. Importers pass
  nothing and estimate. With a hint, the declared duration is the smallest
  multiple of the hint that is at least `duration_min`, the record length a
  regular GOP produces.
- **Estimated by the publisher when not reported.** moq-mux holds that
  timeline's catalog entry until its first record closes, then declares that
  record's duration, rounded up to the timescale. If the first record hits the
  safety ceiling, it declares the ceiling. Estimating at the edge was rejected
  because edges and restarts would disagree. A rolling estimate was rejected
  because it conflicts with a fixed target. The user chose a single sample
  over the longer of two, accepting that an irregular first GOP sets the value.
  The sample is the first record, not the first group, because a group can be
  a single audio packet.
- **Fixed per timeline.** The declared value never changes for a timeline
  track's life. A reconfigure that makes a new timeline track declares its own.
- Sparse timelines (the catalog's own, zero minimum) keep their current
  cutting. Decide what, if anything, they declare, and keep it out of HLS.

Tests: a hinted track declares the smallest multiple of its hint at or past
the minimum. An unhinted track holds its entry until its first record closes,
then declares that record's duration. A first record that hits the ceiling
declares the ceiling. Audio made of one packet per group declares about 4s,
not 20 ms. The declared value survives later, longer records.

## Related

- [Fixed HLS target duration](/quest/m1/archive/hls-target.md) - consumes the declared duration
