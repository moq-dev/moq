# [S] Teleoperation use-case docs

## Goal

`doc/concept/use-case/other.md` stops being a zero-byte stub and becomes the
teleoperation page, with a runnable non-media example beside it.

## Plan

The docs have no non-media tutorial. `rs/moq-native/examples/{chat,clock}.rs`
and `rs/moq-json/examples/telemetry.rs` all publish something that is not audio
or video, but none is presented as the way to carry application data, and
telemetry.rs measures wire savings rather than teaching the shape.

Write the concept page: why control and telemetry belong on the same session as
the video, what the two delivery classes are for, and what a reader is
comparing against (a VPN plus two unmanaged UDP flows). The argument does not
depend on the crate existing, which is why this is ready now and does not block
on it.
