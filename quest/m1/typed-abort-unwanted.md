# [M] Typed producers end an idle on-demand track without stranding a late reader

## Goal

A publisher that serves a requested track through a typed writer (media,
audio, video, or JSON) can end it once nobody wants it, and a reader that
joined in between keeps the track instead of reading `Dropped`, which a relay
forwards as `Internal`. This covers Rust (`moq-mux`, `moq-json`), `moq-ffi`'s
typed producers, and every wrapper over them.

## Plan

[moq-dev/moq#8815](https://github.com/moq-dev/moq/pull/8815) gave
`track::Producer` `abort_unwanted`, and
[moq-dev/moq#8897](https://github.com/moq-dev/moq/pull/8897) mirrored it on
`MoqTrackProducer`. The typed producers expose `demand()` too, but end only by
release, which aborts with `Dropped` (`MoqMediaTrackProducer`'s `Drop` in
`rs/moq-ffi/src/media.rs`, for one), so the race those PRs closed remains for
a media or JSON track accepted from a request.

Recommended: give each typed Rust producer an `abort_unwanted(self, err) ->
Result<(), Self>` that calls its track's and returns itself unchanged while
a reader wants the track, then mirror it in `moq-ffi` the way #8897 did
(`Result<bool, MoqError>` taking an app code), the wrappers, and a test per
wrapper where a reader joins before the abort and keeps reading.
