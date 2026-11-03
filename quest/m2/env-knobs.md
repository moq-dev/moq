# [S] No MOQ_* knobs outside the binaries

## Goal

Only a binary's declared settings use the `MOQ_*` prefix, so a shell that sets
a test, bench, or library knob never trips a binary's unknown-variable refusal
from [moq-dev/moq#6633](https://github.com/moq-dev/moq/pull/6633).

## Plan

- Rename test and bench knobs out of `MOQ_*` (for example `MOQ_SHAPER_SEED`,
  `MOQ_QUIC_SEED`, `MOQ_JSON_SEED`, `MOQ_AUDIO_DEVICE`, `MOQ_BENCH_*`,
  `MOQ_X11_BENCH_LOG`), updating CI and docs that set them.
- moq-video reads `MOQ_VAAPI_DEVICE` and `MOQ_V4L2_*` from the environment as
  a library. Decided 2026-10-25 (recommendation, maintainer delegated): a
  library reads no environment; the binaries that link moq-video declare these
  as settings and pass them in.
