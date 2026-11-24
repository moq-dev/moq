# [S] Delete abort_unwanted and its forwards

## Goal

`track::Producer::abort_unwanted` and every wrapper's forward of it are gone
from the public API, in Rust, `moq-ffi`, and `libmoq`; on-demand publishers
end a track by dropping its producer.

## Plan

Once [a safe drop](/quest/m2/on-demand-drop.md) ends an on-demand track with
no race, `abort_unwanted` only duplicates drop. Delete it and its forwards in
`moq-mux` (each codec importer, the container producer, `import/track.rs`,
`json.rs`), `moq-json` (snapshot, window, stream), `moq-audio`, `moq-video`,
`moq-ffi` (`producer`, `media`, `audio`, `video`, `json`, `demand`) and
`libmoq` (`video`, `audio`, `publish`, `api`), and move each caller to drop.
Internal users (`group.rs`'s `abort_unwanted`, the lite and ietf
subscribers, `origin.rs`) keep whatever crate-private form they still need.

Breaking: it removes public methods, so it lands before the `0.22.0` cut,
which waits on m2: otherwise 0.22 would publish `abort_unwanted`, including
the FFI's ([moq-dev/moq#8897](https://github.com/moq-dev/moq/pull/8897)), only
for the next release to break it. Migration: drop the producer where you
called `abort_unwanted`, and delete the `Err(kept)` retry loop. The FFI and
`libmoq` forms took an `error_code` that recorded `Error::App(code)` as the
track's outcome; it goes with them, since that abort only ever succeeded with
no reader, so no subscriber received the code. An unwanted drop closes the
track cleanly instead.

Decided 2026-11-23 (maintainer): ✅ the cut waits for this quest and
[a safe drop](/quest/m2/on-demand-drop.md) (rejected: cut first and delete in
the next breaking release). This supersedes the cut's 2026-11-16 reason for
landing the FFI's `abort_unwanted` first: it lands, then goes before 0.22
publishes it.

## Required

- [A safe drop](/quest/m2/on-demand-drop.md) - drop must end an on-demand track
  without racing a joining reader before the explicit call can go
