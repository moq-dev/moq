# Migrating moq-archive

How to update your code for each breaking change, newest first.

## Unreleased

### `Producer` replaces `Writer` (#6160)

`Writer`, its `Control`, and `writer::{Config, Retention}` are removed; record with `Producer::new(store, broadcast, producer::Config::default()).run().await` instead.
A producer records every track of one broadcast, found with TRACKS rather than a catalog, and needs no timeline: the layout is per-track objects keyed by group, frame, and timestamp, documented in the crate docs.
Run one producer per broadcast under a key prefix you pick, such as `<path>/@<epoch>`; it stores no epoch, so `Info::epoch` and `Info::with_epoch` are removed.
`writer::Retention` becomes `producer::Dvr`, set with `Config::with_dvr`, and works on untimed tracks too.
`Error::{EpochMismatch, Enrolled, Untimed, Timeline, Closed}` are removed; a failed source listing is `Error::Source`.
The new layout is not readable by `Reader`, which still replays timeline recordings.

### Untimed tracks record untimed (#5720)

`Info::new` and `Info::timescale` take and return `Option<u64>`, and `Error::TimescaleMismatch` carries `Option<u64>` fields.
`segment::Frame::timestamp` is `Option<u64>`.
`Object::decode(buf, timed)` and `Store::get_segments(track, segment, timed)` take whether the track is timed.
Mismatched timing is `Error::Timing`.
Untimed recordings use the version 2 format, which an older version 2 reader refuses.
Pass `Some(timescale)` to `Info::new`, and `info.timescale.is_some()` as `timed`.
