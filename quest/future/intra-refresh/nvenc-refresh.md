# [M] NVENC encodes refresh mode

## Goal

With `Gop::Refresh`, the NVENC backend produces H.264 and HEVC with periodic
intra refresh and a recovery-point SEI at each sweep start, no IDR after the
first frame, and a cut restarts the sweep. A GPU without intra-refresh support
refuses the mode.

## Plan

- `rs/moq-nvenc/src/safe/` exposes what the generated bindings already carry:
  `enableIntraRefresh`, `intraRefreshPeriod`, `intraRefreshCnt`,
  `outputRecoveryPointSEI`, and per-picture `forceIntraRefreshWithFrameCnt`,
  plus the `NV_ENC_CAPS_SUPPORT_INTRA_REFRESH` query. Nothing else from the
  raw API leaks out.
- `rs/moq-video/src/encode/backend/nvenc.rs`: in refresh mode set
  `idrPeriod` and `gopLength` to infinite, `intraRefreshPeriod` to the cycle
  and `intraRefreshCnt` to the cycle minus one, since NVENC requires the count
  strictly below the period, report that count as the sweep length so the
  producer's `warmup` matches the refreshed macroblocks, and translate a cut
  into a forced sweep restart instead of `FORCEIDR`. Check the capability at
  construction and refuse without it.
- Verification needs hardware: no CI runner has an NVIDIA GPU, so run the
  probe by hand, feed the output through the H.264 and H.265 import quests'
  splitters to confirm one group per sweep and the SEI count, and record the
  numbers in the PR. Any test that needs the GPU skips loudly rather than
  reporting success.

## Required

- [Encode config](/quest/future/intra-refresh/encode-config.md) - the `Gop` enum and cut semantics this implements
