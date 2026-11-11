# [S] No MOQ_* knobs outside the binaries

## Goal

Only a binary's declared settings use the `MOQ_*` prefix, so a shell that sets
a test, bench, or library knob never trips a binary's unknown-variable refusal
from [moq-dev/moq#6633](https://github.com/moq-dev/moq/pull/6633).

## Plan

- Rename test and bench knobs out of `MOQ_*` (for example `MOQ_SHAPER_SEED`,
  `MOQ_QUIC_SEED`, `MOQ_JSON_SEED`, `MOQ_AUDIO_DEVICE`, `MOQ_BENCH_*`,
  `MOQ_X11_BENCH_LOG`), updating CI and docs that set them.
- Decided 2026-10-25 (recommendation, maintainer delegated): a library reads
  no environment; the binaries that link it declare their settings and pass
  them in. moq-video's half (`MOQ_VAAPI_DEVICE`, `MOQ_V4L2_*`, and the public
  `moq_video::ENV`) landed separately as `vaapi` and `v4l2` config fields
  before the cut (maintainer, 2026-11-07), so `ENV` never shipped.
