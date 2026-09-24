# [L] moq-mux: catalog delay and jitter measure how far behind the media clock an encoder flushes

## Goal

An original publisher advertises two numbers per rendition, both measured
from the gap between a frame's media timestamp and the wall-clock moment it
handed that frame to the transport (its lateness), the same lateness
`js/watch` `sync.ts` computes on receive, measured one hop earlier:

- `delay`: the rendition's minimum lateness behind the broadcast's earliest
  rendition. A video encoder running 200 ms behind the audio encoder
  advertises `delay: 200` on video and none on audio.
- `jitter`: the spread of the rendition's lateness above its own minimum. A
  fragment flushed at its end, a B-frame held for reordering, and a
  latency-buffered batch all raise it without being special cases.

`delay + jitter` is the worst-case lateness, and neither value is ever
lowered once advertised. `js/watch` sizes playout over the renditions it
subscribes to as `max(delay + jitter) - min(delay)` plus network jitter, and
resizes when that set changes, so dropping a slow track lowers latency.

Only encoders feed the clock; file and pipe imports and the RTMP, SRT, and TS
gateways never do, so an unstable ingest link cannot inflate the catalog. TS
and fragmented MP4 imports retain their clock-free jitter estimates from the
media span of each emitted batch and advertise no `delay`.

## Plan

- **Measurement.** `catalog::Estimator` gains an additive `flush(timestamp,
  now)` observation next to the clock-free `write`. Lateness is
  `now - timestamp`. Each rendition keeps its own baseline, the minimum
  lateness over a sliding window (about 10 s) rather than the lifetime
  minimum, so a media clock that drifts slower than wall time does not ratchet
  forever. The broadcast baseline, on `catalog::Producer` and shared by every
  rendition, is the minimum of those. `delay` is the rendition baseline minus
  the broadcast baseline; `jitter` is `lateness - rendition baseline`. Each is
  reported as its lifetime maximum.
- A faster-than-real-time source flushes early; each frame becomes the new
  minimum and both stay at zero, which is correct for something that is not
  live.
- **Wire.** Add optional `delay` beside `jitter` on video, audio, and text
  renditions in `rs/hang`, `js/hang`, and `drafts/draft-lcurley-moq-hang.md`,
  serialized with `MillisCeil` and zero-as-absent like `jitter`. Additive:
  today's `jitter` never included a cross-track offset. Extend the draft's
  never-lower rule to `delay`, and define both in terms of lateness.
- **Call sites:** the `moq-video` and `moq-audio` encode producers, the capture
  path in `moq import capture`, `moq-gst`, and `libmoq` (so OBS). `js/publish`
  mirrors the measurement in the video and audio encoders and replaces its
  fixed `ceil(1000 / framerate)` and frame-duration hints. `container::Producer`
  does not call it on its own.
- Replace the provisional PTS-gap floor so a decode-order sequence such as
  `0, 120, 40, 80` ms does not permanently advertise its first 120 ms gap
  when the reorder delay is only 80 ms. Preserve the never-lower rule for
  measurements already advertised.
- **Enforce never-lower at the publisher**, not only by convention.
  `js/publish/src/catalog.ts:25-29` already refuses a decrease and a zero for
  audio and video jitter; Rust does not. Add a `moq_mux::Error` for a
  decreased estimate (covering both fields) and return it from
  `Rendition::set`, `Rendition::replace`, and `Rendition::estimate`
  (`rs/moq-mux/src/catalog/tracks.rs`). Extend the `MillisCeil` serialization
  (`rs/hang/src/catalog/millis.rs`) to `TextConfig`
  (`rs/hang/src/catalog/text/mod.rs:124`), and mirror the zero-as-absent
  normalization in `js/hang/src/catalog/text.ts` and the text section of
  `js/publish/src/catalog.ts`. The `js/publish` check covers `delay` as well
  as `jitter`, in every section that carries them.
- **Player.** `Sync` registers each subscribed rendition's `delay` and
  `jitter` and computes `max(delay + jitter) - min(delay)`; it recomputes when
  a rendition registers, unregisters, or its catalog entry rises. When the
  earliest subscribed rendition's `delay` rises, `Sync` re-anchors its
  reference later by that amount instead of leaving it at the old earliest
  arrival, or every other track under-buffers. Rename `Sync`'s own `delay`
  output (the resolved playout total) so it does not collide with the field.
- **Docs.** Update `doc/concept/audio-jitter.md`, the normative playout page,
  with the cross-track rule and the catalog floor it reads.
- **Tests** inject the clock. Cover: a batch flushed at its end reports the
  batch as jitter, a constant offset reports as `delay` on the slower track
  and nothing on the faster, a slow drift stays bounded, a decrease is refused
  on every site, and `Sync` resizes on a subscription change and re-anchors on
  a rising earliest `delay`.

Public API: additive `delay` field in `hang` and `@moq/hang`, a new
`moq_mux::Error` variant, `Sync` output rename in `@moq/watch`. Wire: one
optional catalog field.

## Related

- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - reads `jitter` as its floor, which is now the spread alone
- [Audio time-stretch](/quest/m1/watch-audio-time-stretch.md) - converges audio to a resized target without skipping
- [Data jitter](/quest/m1/data-jitter.md) - feeds the same measurement from JSON and binary tracks
