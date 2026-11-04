# [M] Every moq-mux consumer gets the catalog warmup from one entry point

## Goal

A moq-mux container consumer is built from a subscription delay and the
catalog rendition's `warmup` in one call. That call widens the subscription
and configures the consumer, so a caller can't forget the warmup or apply it
in the wrong order. Every raw consumer uses it: moq-audio, moq-c, moq-ffi's
`MoqMediaTrackConsumer`, and moq-gst's `moqsrc`. moq-ffi's raw consumer and
`moqsrc` then start early enough for a warmed-up decoder, as moq-c's
`moq_consume_audio`/`video` already do.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7723](https://github.com/moq-dev/moq/pull/7723)): milestone m1.
#7723's two follow-ups are one quest, since the simpler entry point is how
the two missing consumers get the warmup.

- Today (`rs/moq-mux/src/container/consumer.rs:340-371`, and
  `rs/moq-mux/MIGRATE.md:18`) the caller must add the warmup to the
  subscription's `delay.max` before subscribing. It must then call
  `with_warmup`, which carves the warmup back out, and call it before
  `with_delay`. moq-audio (`decode/consumer.rs:245`) and moq-c
  (`consume.rs:540-550`) do this by hand.
- Missing today: `rs/moq-ffi/src/media.rs:1717-1748`
  (`MoqMediaTrackConsumer::subscribe`) and
  `rs/moq-gst/src/source/imp.rs:1649-1673` subscribe with the caller's delay
  and never call `with_warmup`. The deleted moq-c quest
  (`quest/m1/c/raw-consume-warmup.md`, removed in `cd7f9a9a38`) wrongly
  assumed moq-ffi already matched.
- Shape: one moq-mux constructor or builder takes the track or subscription
  request, the `Delay`, and the warmup. It returns the widened subscription
  and a consumer configured to match, or subscribes itself. Remove the
  order-sensitive `with_warmup` + `with_delay` pair from the public API,
  and record the break in `MIGRATE.md`.
- moq-video has its own two-decoder scheme (`decode/consumer.rs:176-185`).
  Move it onto the new entry point if it fits. If not, record why here.
- Test each consumer: a test that starts a track with a nonzero catalog
  warmup fails without that consumer's adoption.
- Builds on [moq-dev/moq#7697](https://github.com/moq-dev/moq/pull/7697)
  (merged), which replaced the bindings' `max_delay` with
  `Delay { min, max }` and edited `rs/moq-ffi/src/media.rs`.

Public API: moq-mux's container consumer setup changes (breaking). The
bindings' subscribe method stays unchanged. If the maintainer chooses
optional FFI catalog `warmup` fields below, update the public records,
every language wrapper, and moq-ffi's `MIGRATE.md` too. Wire: none.

Implementation evidence, 2026-11-04:

- `MoqAudio` and `MoqVideo` in `rs/moq-ffi/src/media.rs` do not contain
  `warmup`. `convert_catalog` discards it before a rendition reaches
  `MoqMediaTrackConsumer::subscribe`. Consumer adoption alone cannot recover
  the catalog value. The subscribe method can stay unchanged, but carrying
  the value through its rendition requires a public catalog-record change.
- A temporary paused-time regression on unchanged implementation code
  converted native audio and video catalogs with 300 ms warmup, then passed
  those renditions to the public raw FFI subscribe method with a 200 ms
  delay. Both producer-side subscription maxima were 200 ms, rather than
  the required 500 ms. The temporary regression was removed after
  reproduction; no production code changed. These are recorded observations,
  not a regression checked into the suite. Permanent adoption regressions
  remain required by the Plan.
- `moqsrc` also discards warmup while constructing its private `Shape` and
  subscribes using only `Session::max_delay`. Carrying the value privately
  would not require a GStreamer property change.
- moq-video's second lane adds warmup to both delay bounds and keeps it as
  steady-state lateness tolerance, with a one-group lookback for `Latest`.
  Its container warmup must remain zero if it adopts the shared setup;
  subtracting the catalog warmup would change that lane's skip behavior.

Decided 2026-11-04 (maintainer, on [moq-dev/moq#7864](https://github.com/moq-dev/moq/pull/7864)):
how catalog warmup reaches the raw FFI consumer.

- ✅ Add public optional `warmup` fields to `MoqAudio` and `MoqVideo`, so
  `convert_catalog` keeps a rendition's warmup and
  `MoqMediaTrackConsumer::subscribe` receives it with the rendition.
- Not chosen: resolve warmup from the broadcast catalog inside the raw
  subscriber, which needs a policy for standalone renditions and missing
  catalogs.
