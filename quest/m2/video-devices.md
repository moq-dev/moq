# [S] moq-video takes devices, not environment variables

## Goal

moq-video reads no environment. The VA-API render node and the V4L2 encoder
and decoder device paths are Linux-only `Option<PathBuf>` fields on
`encode::Config` and `decode::Config`, next to `encode::Config::input`.
`moq_video::ENV` and the reads in `rs/moq-video/src/frame/vaapi.rs` and
`rs/moq-video/src/v4l2.rs` are gone. moq-cli declares `MOQ_VAAPI_DEVICE`,
`MOQ_V4L2_ENCODER`, and `MOQ_V4L2_DECODER` as its own settings and passes
them in, so moq-ffi, studio, and other embedders can pick a device in code.

## Plan

Found by the 2026-11-07 API shape audit (native-media, scored 3/10).
[env-knobs](/quest/m4/env-knobs.md) decided on 2026-10-25 that "a library
reads no environment", but it lands after the cut while `ENV` is new public
surface since release. This quest takes moq-video's half before the cut so
`ENV` never ships; env-knobs keeps the test and bench knobs. Tests stop
mutating process environment (`frame/vaapi.rs`). Decide in the PR whether
moq-ffi and moq-c expose the device fields now or when a binding consumer
asks; the audit found none.
