# Migrating moq-archive

How to update your code for each breaking change, newest first.

## Unreleased

### Recording format version 3 replaces version 1

Release wrote version 1; the current codecs refuse it. Start a new recording prefix and retain an older reader for existing version 1 recordings.
`Info::priority` and `Info::new` take signed `i8` priority, with 0 unset, rather than the moq-lite byte. Subtract 128 from an old byte to preserve its wire priority.
`Info::timescale` is optional, and `Info::new` takes `Some(timescale)` for timed tracks or `None` for untimed tracks.

### Recording is driven by a broadcast producer

`Producer::new(store, broadcast, producer::Config::default()).run().await` records one broadcast, discovering its tracks with TRACKS instead of requiring a catalog or timeline.
Run one producer per broadcast under a new prefix such as `<path>/@<epoch>`; configure retention with `producer::Dvr` and `Config::with_dvr`.
The current `Reader` still replays timeline recordings, not this producer's new per-track layout; it does not provide an upgrade reader for new recordings.

### Objects are keyed by timeline segment

`Key::Groups`, `Key::groups`, and the `Store` group-range operations are removed. Use `Key::Segments` / `Key::segments` and `Store::get_segments` for timeline record sequences.
`Error::Bounds { smallest, largest }` is `Error::Span`; update exhaustive matches.
`segment::Frame::timestamp` is optional. Pass the track's timing to `Object::decode(buf, timed)` and `Store::get_segments(track, segment, timed)`; a timing mismatch is `Error::Timing`.
