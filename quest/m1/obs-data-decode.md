# [S] Bindings decode media tracks as media

## Goal

moq-ffi's `MoqMediaTrackConsumer` decodes a video or audio rendition with its
own kind, so the empty marker that ends a video group never reaches a binding
as an empty frame, and OBS plays every group.

## Plan

Found by the OBS decode-timing quest
([moq-dev/moq#6711](https://github.com/moq-dev/moq/pull/6711)), not yet
reproduced: `rs/moq-ffi/src/media.rs:1253` builds every container with
`container::Kind::Data`. FFmpeg treats an empty packet as end of stream, so OBS
likely drops every other group on streams without B-frames.

- Reproduce in an OBS source test or a moq-ffi test that reads two video
  groups through the consumer.
- Pass the rendition's video or audio kind into `media_container`.
- Run `just test interop --all`.

Public API: none expected. Wire: none.
