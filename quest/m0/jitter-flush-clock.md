# [M] moq-mux: catalog jitter measures how far behind the media clock an encoder flushes

## Goal

An original publisher advertises `jitter` as the largest gap it has ever
seen between a frame's media timestamp and the wall-clock moment it handed
that frame to the transport, relative to the smallest such gap: the same
lateness `js/watch` `sync.ts` computes on receive, measured one hop earlier.
A fragment flushed at its end, a B-frame held for reordering, a slow video
encoder, and a latency-buffered batch all raise it without being special
cases, and a video encoder running 200 ms behind the audio encoder shows up
as 200 ms on the video rendition. Only encoders feed the clock; file and pipe
imports and the RTMP, SRT, and TS gateways never do, so an unstable ingest
link cannot inflate the catalog. TS and fragmented MP4 imports retain their
clock-free estimates from the media span of each emitted batch.

## Plan

- `catalog::Estimator` gains an additive `flush(timestamp, now)` observation
  next to the clock-free `write`. Lateness is `now - timestamp`; the baseline
  is the minimum lateness over a sliding window (about 10 s) rather than the
  lifetime minimum, so a media clock that drifts slower than wall time does
  not ratchet the value forever. The reported jitter is the lifetime maximum
  of `lateness - baseline`, which keeps the draft's never-lower rule.
- The baseline lives on `catalog::Producer` and is shared by every rendition
  in the broadcast, so a constant offset between two encoders is reported on
  the slower one instead of absorbed per track.
- A faster-than-real-time source flushes early; each frame becomes the new
  minimum and the measurement stays at zero, which is correct for something
  that is not live.
- Call sites: the `moq-video` and `moq-audio` encode producers, the capture
  path in `moq-cli publish`, `moq-gst`, and `libmoq` (so OBS). `js/publish`
  mirrors the measurement in the video and audio encoders and replaces its
  fixed `ceil(1000 / framerate)` and frame-duration hints. `container::Producer`
  does not call it on its own.
- Replace the provisional PTS-gap floor so a decode-order sequence such as
  `0, 120, 40, 80` ms does not permanently advertise its first 120 ms gap
  when the reorder delay is only 80 ms. Preserve the never-lower rule for
  measurements already advertised.
- Tests inject the clock. Cover: a batch flushed at its end reports the
  batch, a constant offset reports zero on one track and the offset on the
  other track sharing the baseline, and a slow drift stays bounded.

## Related

- [Auto latency](/quest/m0/3477-watch-auto-latency.md) - reads this field as its floor
- [Encoder lag](/quest/m0/publish-audio-lag-measure.md) - the js/publish measurement this replaces the hint with
