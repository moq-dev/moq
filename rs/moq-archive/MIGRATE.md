# Migrating moq-archive

How to update your code for each breaking change, newest first.

## Unreleased

### `Consumer` replaces `Reader`

`Reader`, `reader::Config`, and the timeline layout it read are removed; serve an archive with `Consumer::open(store, broadcast, consumer::Config::default()).await?` and then `run().await`.
`open` creates every stored track on the `broadcast::Producer` you pass, so announce it afterwards, with an epoch you pick; `run` follows a growing archive until its `.end` and serves until the broadcast closes, replacing `Reader::{track, refresh, finish, serve}`.
`Consumer` reads only the per-track layout `Producer` writes, version 2 or 3; an old timeline recording holds keys it does not recognize and is refused.
The timeline layout's API goes with it: `Key`, `Frame`, `Group`, `Object`, the `segment` module, `store::list`, and `Store::{path, put_segments, get_segments, timelines, delete, list, list_paginated}`; use the `object_store` API on `Store::inner` for raw access.
`Store::put_info` returns `()`.
`.info` refuses an `epoch` member, which an older `Writer` stored.
`Error::{Span, Directory, Pagination}` are removed.

### Recording format version 3 stores a signed priority (#6142)

`Info::priority`, `Info::new`, and `Error::Priority` use `i8`, higher first, with 0 unset.
`VERSION` is 3, whose `.info` stores that `i8` as written; version 2 stored the moq-lite byte, `priority + 128`.
Readers still read version 2 recordings, converting the byte, so a stored 127 reads as -1; a decoded version 2 `Info` keeps `version: 2` and refuses to `encode`.
A `Producer` refuses to resume a version 2 recording with `Error::Version(2)`, so start a new prefix.

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
