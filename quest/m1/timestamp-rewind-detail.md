# [XS] A timestamp rewind names the frame and the edge

## Goal

When `container::Producer::write` refuses a frame below the live edge, the
error says which timestamp it refused and where the edge was, so a report like
#4635 diagnoses itself from the error text alone.

## Plan

`container::TimestampRewind` (`rs/moq-mux/src/container/mod.rs`) is a unit
struct, raised at `rs/moq-mux/src/container/producer.rs` when
`frame.timestamp < live_edge`. Give it `timestamp` and `edge` fields and put
both, in microseconds, in its message, for example "frame timestamp 1790802494898431
µs is below the live edge 1790802494898432 µs". moq-ffi surfaces the message
unchanged.

#4635 could not be reproduced: FLAC and AAC share this path, and strictly
increasing timestamps with a cut per frame pass. A cut per frame makes every
frame a group start, so it refuses any backward step, while a dip inside an
open group is tolerated. Test: a FLAC `import::Track` with `cut(None)` per
frame and one frame 1 µs back reports both values.

Public API: a unit struct gains fields, so breaking, on `dev`. Wire: none.

## Related

- [#4635](https://github.com/moq-dev/moq/issues/4635) - the report this makes diagnosable; it stays open until the reporter's sequence is known
