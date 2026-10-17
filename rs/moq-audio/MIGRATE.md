# Migrating moq-audio

How to update your code for each breaking change, newest first.

## Unreleased

### Decoded reads carry encoder ends (#5687)

`decode::Consumer::read()` returns `decode::Read` events instead of bare frames: `Frame { discontinuity, frame }` and `End { discontinuity, end }`; `None` still means the track is finished.
An `End` can be followed by final PCM with the same `discontinuity` before a fresh encoder bumps it, and a bump with no `End` before it is a loss.
Binding audio frame APIs keep their existing shape.
Play `decode::Read::Frame { frame, .. }` as before, and trim after `decode::Read::End { end, .. }`.

### Capture handles are `Control` and `Capture`

`encode::Publication` is `encode::Control`, and `encode::PublicationOptions` is `encode::Capture`, without its `clock` field: the driver stamps on `catalog.clock()`.
Build it from `encode::Capture::default()`, setting `capture` and `encode`.

### `reset_epoch` is `reanchor` (#5146)

"Epoch" means only the publisher epoch, so the audio re-anchor is renamed.

### Decoder staleness is `max_delay`

`decode::Options::max_age` is `max_delay`, and `decode::Consumer::max_age()` is `max_delay()`.
