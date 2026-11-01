# Migrating moq-audio

How to update your code for each breaking change, newest first.

## Unreleased

### `decode::Options` takes a `Delay` and `estimate` ([#7311](https://github.com/moq-dev/moq/pull/7311))

`decode::Options::max_delay: Duration` and the playout `delay: Option<Duration>` are one `delay: decode::Delay` (`moq_mux::container::Delay`) plus `estimate: bool`.
`delay.max` is the old `max_delay`, and `delay.min` is the playout delay, where a cold join starts.
A fixed `delay: Some(d)` is `delay.min = d` with `estimate: false`. The default `None` (estimate from arrivals) is `estimate: true`, which now never goes below `delay.min`.

### `Level` is at the crate root

`encode::Level` is `moq_audio::Level`, since the program bus's meters report it too.

### Decoded reads carry gaps and encoder ends (#5687, #6475)

`decode::Consumer::read()` returns `decode::Read` events instead of bare frames, the same shape as `moq_video::decode::Read`: `Frame(frame)`, `Gap { timestamp, duration }` for lost media, and `End { end }` where the publisher stopped its encoder; `None` still means the track is finished.
Frames after an `End` that start before `end` are the stopped encoder's final PCM; the first at or past it comes from a fresh encoder.
Binding audio frame APIs keep their existing shape.
Play `decode::Read::Frame(frame)` as before, restart playback after a `Gap`, and expect the end of the current timeline at `End { end }`.

### Capture handles are `Control` and `Capture`

`encode::Publication` is `encode::Control`, and `encode::PublicationOptions` is `encode::Capture`, without its `clock` field: the driver stamps on `catalog.clock()`.
Build it from `encode::Capture::default()`, setting `capture` and `encode`.

### `reset_epoch` is `reanchor` (#5146)

"Epoch" means only the publisher epoch, so the audio re-anchor is renamed.

### Decoder staleness is `max_delay`

`decode::Options::max_age` is `max_delay`, and `decode::Consumer::max_age()` is `max_delay()`.
