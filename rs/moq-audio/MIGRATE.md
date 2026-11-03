# Migrating moq-audio

How to update your code for each breaking change, newest first.

## Unreleased

### Capture is one producer with independent consumers

Open `capture::Producer::open(config).await?` once and create a consumer with
`capture.consume()` for each destination. Pass one to `bus.capture(consumer, strip)?`
and another to `encode::Capture::new(consumer)`, then set its `encode` and
`on_demand` fields. Drive `capture.run()` alongside the bus and encoder futures.
This replaces both device-opening entry points; `encode::Capture::default()`
and its `capture: Config` field are gone. The audio rendition is registered
only after the capture has opened and its actual input format is known.

`encode::Control::start` and `stop` control only that encoder, and `replace`
takes another capture consumer and returns an error, keeping the current one, when
its layout cannot be converted into the track's input. They never open or release
the device. A capture
failure sets `Status::Failed` at once; the driver keeps serving retained
on-demand audio until it is served or expires, then returns the failure.
Recovery requires a new capture and publication. There is no automatic device reopen. Dropping the capture
producer stops every consumer; dropping one consumer leaves the others running.
Capture readers share immutable f32 buffers, a fixed `encode::Input`, an `Instant`
for the first sample, and a gap flag that also reports that reader's overruns.

Device channel counts above stereo are now consistently `Layout::Discrete`:
a microphone array does not declare surround-speaker positions. A stereo bus
refuses those inputs instead of guessing a surround downmix. Request mono or
stereo through `capture::Config::channels` when the device supports it, or map
the known channels explicitly before writing a `Bus::channel`. The encoder refuses
them too: `encode::Control::new` fails for such a consumer, since its default
stereo config (and Opus in any config) cannot take a discrete layout. Publish a
microphone array only when it delivers mono or stereo, or with an
`encode.config.layout` set to that same `Layout::Discrete` for a codec that carries it.

### `decode::Options` takes a `Delay` and `estimate` ([#7311](https://github.com/moq-dev/moq/pull/7311), [#7597](https://github.com/moq-dev/moq/pull/7597))

`decode::Options::max_delay: Duration` and the playout `delay: Option<Duration>` are one `delay: decode::Delay` (`moq_net::track::Delay`) plus `estimate: bool`.
`delay.max` is the old `max_delay`, and `delay.min` is the playout delay, where a cold join starts.
A fixed `delay: Some(d)` is `delay.min = Some(d)` with `estimate: false`. The default `None` (estimate from arrivals) is `estimate: true`, which now never goes below `delay.min`.

### `Level` is at the crate root

`encode::Level` is `moq_audio::Level`, since the program bus's meters report it too.

### Decoded reads carry gaps and encoder ends (#5687, #6475)

`decode::Consumer::read()` returns `decode::Read` events instead of bare frames, the same shape as `moq_video::decode::Read`: `Frame(frame)`, `Gap { timestamp, duration }` for lost media, and `End { end }` where the publisher stopped its encoder; `None` still means the track is finished.
Frames after an `End` that start before `end` are the stopped encoder's final PCM; the first at or past it comes from a fresh encoder.
Binding audio frame APIs keep their existing shape.
Play `decode::Read::Frame(frame)` as before, restart playback after a `Gap`, and expect the end of the current timeline at `End { end }`.

### Capture handles are `Control` and `Capture`

`encode::Publication` is `encode::Control`, and `encode::PublicationOptions` is `encode::Capture`, without its `clock` field: the driver stamps on `catalog.clock()`.
Build it from `encode::Capture::new(consumer)`, then set `encode`.

### `reset_epoch` is `reanchor` (#5146)

"Epoch" means only the publisher epoch, so the audio re-anchor is renamed.

### Decoder staleness is `max_delay`

`decode::Options::max_age` is `max_delay`, and `decode::Consumer::max_age()` is `max_delay()`.
