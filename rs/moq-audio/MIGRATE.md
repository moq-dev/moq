# Migrating moq-audio

How to update your code for each breaking change, newest first.

## Unreleased

### `encode::Settings` is `encode::Config`

`encode::Settings` is `encode::Config`, matching `decode::Config` and `moq_video::encode::Config`.
`encode::Options::settings` is `config`, and `encode::Encoder::settings()` is `config()`.

### `Kind::Named` is removed

`encode::Kind::Named` and `decode::Kind::Named` only checked the codec, which the encoder already takes from `Config::codec` and the decoder from the catalog.
Drop the variant and use `Kind::Auto`.

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
