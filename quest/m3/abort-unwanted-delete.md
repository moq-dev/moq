# [S] Delete abort_unwanted and its forwards

## Goal

`track::Producer::abort_unwanted` and every wrapper's forward of it are gone
from the public API, in Rust, `moq-ffi`, and `libmoq`; on-demand publishers
end a track by dropping its producer.

## Plan

Once [a safe drop](/quest/m3/on-demand-drop.md) ends an on-demand track with
no race, `abort_unwanted` only duplicates drop. Delete it and its forwards in
`moq-mux` (each codec importer, the container producer, `import/track.rs`,
`json.rs`), `moq-json` (snapshot, window, stream), `moq-audio`, `moq-video`,
`moq-ffi` (`producer`, `media`, `audio`, `video`, `json`, `demand`) and
`libmoq` (`video`, `audio`, `publish`, `api`), and move each caller to drop.
Internal users (`group.rs`'s `abort_unwanted`, the lite and ietf
subscribers, `origin.rs`) keep whatever crate-private form they still need.

Breaking: it removes public methods, so it rides the next breaking release
after it lands; the cut after m2 sorts it there. Migration: drop the producer
where you called `abort_unwanted`, and delete the `Err(kept)` retry loop.

## Required

- [A safe drop](/quest/m3/on-demand-drop.md) - drop must end an on-demand track
  without racing a joining reader before the explicit call can go
