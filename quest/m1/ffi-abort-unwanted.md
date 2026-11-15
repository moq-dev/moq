# [M] The bindings end an idle on-demand track without stranding a late reader

## Goal

`moq-ffi`'s `MoqTrackProducer` and every SDK wrapper over it (Go, Python,
Swift, Kotlin, Dart) offer `abort_unwanted`, mirroring Rust's
`track::Producer::abort_unwanted`. An on-demand publisher in any language can
end a track once its demand reports nobody wants it, and a reader that joined
in between keeps the track instead of reading an error.

## Plan

Today a binding's on-demand publisher waits on `MoqTrackDemand::wanted_changed`
for `None` and then drops or `abort`s the track. Either way, a reader that
joins in that window reads `Dropped` or the abort error, which a relay forwards
as `Internal`.

Recommended shape (from the review of the Rust change):
`pub fn abort_unwanted(&self, error_code: u16) -> Result<bool, MoqError>` on
`MoqTrackProducer`. It takes the `phase` lock, takes the `Open` or `Finished`
producer, and calls `raw.abort_unwanted(moq_net::Error::App(error_code))`.
`Ok(())` leaves the phase `Closed` and returns true. `Err(raw)` restores the
phase and returns false, so the caller keeps serving. `Closed` fails the way
`abort` does. Add a note on `MoqTrackDemand::wanted_changed` matching the Rust
doc, regenerate the Dart bindings, add the wrapper methods, and cover each
wrapper with a test where a reader joins before the abort and keeps reading.
