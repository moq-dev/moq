# [M] libmoq: fetch_group and the video format knob

## Goal

A C embedder can fetch one cached group by sequence, and can ask the video
decoder for the pixel format and target size it wants instead of always
receiving packed I420 at stream resolution. Both are additive on `moq.h`, so
they ship on main.

## Plan

- Fetch: moq-ffi's `MoqTrackConsumer::fetch_group`
  (rs/moq-ffi/src/consumer.rs:271) has no C mirror; `rs/libmoq/src/api.rs`
  has no `fetch` symbol at all. Add a group fetch that delivers through the
  same frame callback, handle, and terminal-status contract as
  `moq_consume_track`, decoded through the container or raw like the FFI.
- Video format knob: `moq_consume_video` (api.rs:2366-2380) takes a catalog
  index and a max age and delivers encoded frames; decoding runs on
  `moq-video` with NVIDIA and VAAPI on (rs/libmoq/Cargo.toml:32) behind
  `moq_decode_video` (rs/libmoq/src/video.rs:618). Its decode-side config
  `moq_video_decoder_output` (video.rs:125-140) carries only `max_age_ms`: output is always tightly packed I420 at
  stream resolution. Add the pixel format and target size there, which is
  what the struct was left in place for. `moq play` (rs/moq-cli, the `play`
  feature) is the worked example of the shape.

Each addition regenerates `moq.h`, touches `cpp/obs/src` only if used, and
updates `doc/lib/c/index.md`. That page's capability list (:39) already
claims group fetch for C; the fetch symbol makes it true.

## Related

- [#2152](/quest/m1/2152-libmoq-c-abi-catch-up-with-the-moq-ffi-surface.md) - the dev half: dynamic track serving and server-side accept
