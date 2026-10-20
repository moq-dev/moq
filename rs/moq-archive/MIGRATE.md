# Migrating moq-archive

How to update your code for each breaking change, newest first.

## Unreleased

### Untimed tracks record untimed (#5720)

`Info::new` and `Info::timescale` take and return `Option<u64>`, and `Error::TimescaleMismatch` carries `Option<u64>` fields.
`segment::Frame::timestamp` is `Option<u64>`.
`Object::decode(buf, timed)` and `Store::get_segments(track, segment, timed)` take whether the track is timed.
`Error::Untimed` now means a DVR cannot record an untimed track; mismatched timing is `Error::Timing`.
Untimed recordings use the version 2 format, which an older version 2 reader refuses.
Pass `Some(timescale)` to `Info::new`, and `info.timescale.is_some()` as `timed`.
