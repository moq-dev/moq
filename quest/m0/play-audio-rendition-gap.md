# [S] play: an audio rendition switch costs a delay of silence

## Goal

Swapping audio renditions in `moq play` is inaudible, rather than leaving a
silence the size of `--delay`.

`Playback` in `rs/moq-cli/src/play/playback.rs` runs one task per kind, so
`Media::play` cannot start a replacement audio rendition until the retired
`play_audio` returns. That task ends by draining its sink, which by design takes
the full depth it was holding, and the replacement then opens a fresh sink that
holds its own depth before the first sample sounds. The two buffers run in
series, so the gap is one `--delay`: inaudible at the 100 ms default, ten
seconds at the ceiling.

The same shape costs 50 ms today on a mid-track sink reset (a hole too large to
play through, or a publisher rewind), which is why it went unnoticed.

## Plan

`playback::Engine` already mixes several sinks on one device, so overlapping the
two is a mixing problem rather than a device one. What it is not is a free
change to `Playback`, whose one-task-per-kind invariant is what makes retirement
handling readable today.

Worth weighing, and the choice is A/V policy:

- Let the replacement start while the retired sink drains, and let the engine
  mix the overlap. Closest to what a rendition switch should sound like, and the
  largest change to `Playback`.
- Hand the replacement the retired sink rather than opening a new one, so the
  ring is already at depth. Keeps one task per kind, but only works when the
  rate and channel count match.

Retiring a rendition mid-track is the case to reproduce: a transcode ladder
resizing under a source that changed resolution is the one that happens in
practice.

## Related

- [Play tune-in backpressure](/quest/m0/play-tunein-backpressure.md) - the other place a wide `--delay` costs more latency than it asked for
