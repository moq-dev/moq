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

- **Landed (#3940):** `jitter` measured at encoder flush. `Estimator::flush`,
  `container::Producer::flush`, and codec importer forwarding; each rendition
  keeps its own 10 s sliding minimum and advertises the lifetime maximum spread
  above it. The `moq-video` and `moq-audio` encoders (so `moq import capture`),
  libmoq (so OBS), moq-ffi and its wrappers, and the `js/publish` encoders call
  it. The provisional PTS-gap floor is gone, and `moq_mux::Error::JitterDecreased`
  plus zero-as-absent text jitter enforce never-lower in Rust and JS.
  `moq-gst` pads opt in with `encoder=true`; imports stay clock-free.
  What remains below is `delay` and the player. A seek or pause resetting the
  baseline from moqsink and the bindings is
  [Import discontinuity](/quest/m1/import-discontinuity.md), including a
  `moq-gst` encoder pad across a `PLAYING -> PAUSED -> PLAYING` cycle (running
  time stops, the wall clock does not) and a flushing seek.
- **Measurement.** Lateness is `now - timestamp`, observed by the existing
  `flush` calls, so no call site changes. Each rendition keeps its own
  baseline, the minimum lateness over a sliding window (about 10 s), so a media
  clock that drifts slower than wall time does not ratchet forever. The
  broadcast baseline, on `catalog::Producer` and shared by every rendition
  that flushes, is the minimum of those; the per-rendition `Baseline` epochs
  must become one shared epoch for the subtraction to mean anything. `delay` is
  the rendition baseline minus the broadcast baseline, reported as its lifetime
  maximum. Renditions that never flush advertise no `delay`.
- **Open:** lifetime maxima taken against a sliding baseline stop sharing an
  origin when the earliest rendition changes. If A starts at 0 and B at
  200 ms, B keeps `delay: 200`; if A then drifts to 500 ms, A advertises 300
  and `Sync` computes `300 - 200 = 100` while the tracks are 300 ms apart.
  Options:
  - *No subtraction (recommended).* Keep the sliding baselines and never-lower,
    and change the player rule to `max(delay + jitter)` over the subscribed
    renditions, dropping `- min(delay)`. The subscribed renditions' true spread
    is measured from an earliest subscribed baseline no earlier than the
    broadcast baseline, so it never exceeds the largest advertised `delay`,
    and the catalog alone can never under-buffer. The cost is over-buffering by
    `min(delay)` when the broadcast's earliest rendition is not subscribed
    (a video-only viewer of a broadcast whose audio leads by 200 ms pays
    200 ms). Dropping a slow track still lowers latency. The draft says a
    consumer MUST NOT subtract `delay` values across renditions.
  - *Fixed common origin.* Measure every `delay` from the broadcast's first
    lateness. Exact subtraction, but common drift raises every rendition
    together, so values grow without bound and the catalog republishes for the
    life of the broadcast; the problem the sliding window exists to avoid.
  - *Coordinated rebasing.* Advertise each `delay` as its current value against
    the current broadcast baseline and let it fall. Exact, but `delay` gives up
    never-lower, the catalog churns as baselines move, and the player must
    shrink safely.
  - *No `delay` field.* The player measures each subscribed track's own
    arrival baseline and sizes by their spread. No wire change, and it also
    covers gateway and ingest offsets, but a track's offset is unknown until its
    first frames arrive, so subscribing to a slower rendition glitches once.
  Every option also needs the player's reference to follow the earliest
  subscribed track's current arrival rather than its lifetime minimum, since
  `Sync.received` only ever lowers it; that is receiver-side and belongs with
  the arrival minimum the [audio jitter target](/quest/m0/audio-jitter-target/README.md)
  already expires.
- A faster-than-real-time source flushes early; each frame becomes the new
  minimum and both stay at zero, which is correct for something that is not
  live.
- **Wire.** Add optional `delay` beside `jitter` on video, audio, and text
  renditions in `rs/hang`, `js/hang`, and `drafts/draft-lcurley-moq-hang.md`,
  serialized with `MillisCeil` and zero-as-absent like `jitter`. Additive:
  today's `jitter` never included a cross-track offset. Extend the draft's
  never-lower rule to `delay` (unless rebasing wins), along with
  `moq_mux::Error::JitterDecreased` and the `js/publish/src/catalog.ts` check.
- **Player.** `Sync` registers each subscribed rendition's `delay` and
  `jitter` and recomputes when a rendition registers, unregisters, or its
  catalog entry rises. Rename `Sync`'s own `delay` output (the resolved
  playout total) so it does not collide with the field. #3954 on the
  [audio jitter target](/quest/m0/audio-jitter-target/README.md) line changes
  what `register()` takes, so build on whichever lands first.
- **Docs.** Update `doc/concept/audio-jitter.md`, the normative playout page,
  with the cross-track rule and the catalog floor it reads.
- **Tests** inject the clock. Cover: a constant offset reports as `delay` on
  the slower track and nothing on the faster, a slow common drift stays
  bounded, the earliest rendition changing does not under-buffer, a decrease is
  refused on every site, and `Sync` resizes on a subscription change.

Public API: additive `delay` field in `hang` and `@moq/hang`, the
decreased-estimate error extended to `delay`, `Sync` output rename in
`@moq/watch`. Wire: one optional catalog field.

## Related

- [Audio jitter target](/quest/m0/audio-jitter-target/README.md) - reads `jitter` as its floor, which is now the spread alone
- [Audio time-stretch](/quest/m1/watch-audio-time-stretch.md) - converges audio to a resized target without skipping
- [Data jitter](/quest/m1/data-jitter.md) - feeds the same measurement from JSON and binary tracks
