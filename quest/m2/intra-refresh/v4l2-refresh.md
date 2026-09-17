# [S] V4L2 encodes refresh mode

## Goal

With `Gop::Refresh`, the V4L2 backend asks the hardware for periodic intra
refresh and no IDR after the first frame; a driver that lacks the control
refuses the mode. Groups come from the producer's frame count, so a driver
that emits no recovery-point SEI still forms one group per sweep.

## Plan

- `rs/moq-video/src/encode/backend/v4l2.rs`: import
  `V4L2_CID_MPEG_VIDEO_INTRA_REFRESH_PERIOD` (falling back to
  `V4L2_CID_MPEG_VIDEO_CYCLIC_INTRA_REFRESH_MB` as macroblocks per frame
  derived from the picture size and the cycle) from the sys crate, set
  `GOP_SIZE` to zero for an unbounded GOP, and refuse the mode when the driver
  rejects both controls, unlike the self-disabling `keyframes` fallback used
  for forced keyframes.
- V4L2 has no control to restart a sweep, so a cut in refresh mode is honoured
  at the next sweep start; the producer's count stays aligned to the driver's
  period from the first frame. Say so in the backend doc.
- Verify on the hardware the backend already targets and note whether the
  driver emits the SEI; the hang side does not need it.

## Required

- [Encode config](/quest/m2/intra-refresh/encode-config.md) - the `Gop` enum and cut semantics this implements
