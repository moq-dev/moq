# [S] One frame rate name

## Goal

Rust's `moq_video::Rate` is `moq_video::FrameRate`, the name the bindings
already use (`moq_video_frame_rate` in C, `VideoFrameRate` in C++), so the
type has one name across Rust, JS, and the bindings.

## Plan

[Binding encoder config](/quest/m1/binding-encoder-config.md) names the
binding type `FrameRate` and leaves the core `Rate`
(`rs/moq-video/src/rate.rs`) as is. Rename the core type and its uses in
moq-video, moq-transcode, moq-cli, moq-boy, moq-c, and moq-ffi (about 137
references), with `MIGRATE.md` entries for each published crate.

Decided 2026-10-23 (maintainer): rename the core type, not the bindings.

Public API: breaking rename in moq-video. Wire: none.

## Required

- [Binding encoder config](/quest/m1/binding-encoder-config.md) - introduces the binding `FrameRate`
