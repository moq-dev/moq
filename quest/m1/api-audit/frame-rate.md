# [S] One frame rate name

## Goal

Rust's `moq_video::Rate` is `moq_video::FrameRate`, the name the bindings
already use (`moq_video_frame_rate` in C, `VideoFrameRate` in C++), so the
type has one name across Rust, JS, and the bindings.

## Plan

The bindings name the type `FrameRate` and leave the core `Rate`
(`rs/moq-video/src/rate.rs`) as is. Rename the core type and its uses in
moq-video, moq-transcode, moq-cli, moq-boy, moq-c, and moq-ffi (about 137
references); the Impact bullet says how to migrate each published crate.

Decided 2026-10-23 (maintainer): rename the core type, not the bindings.

Public API: breaking rename in moq-video. Wire: none.
