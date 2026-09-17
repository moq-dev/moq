# [S] V4L2 encodes refresh mode

## Goal

With `Gop::Refresh`, the V4L2 backend asks the hardware for periodic intra
refresh and no IDR after the first frame; a driver that lacks the control
refuses the mode. Groups come from the producer's frame count, so a driver
that emits no recovery-point SEI still forms one group per sweep. V4L2 has no
control that restarts a sweep, so a mid-cycle `cut()` is refused on this
backend rather than deferred; the only cut it honours is the implicit one on a
fresh encoder, whose first frame starts the first sweep.

## Plan

- `rs/moq-video/src/encode/backend/v4l2.rs`: import
  `V4L2_CID_MPEG_VIDEO_INTRA_REFRESH_PERIOD` (falling back to
  `V4L2_CID_MPEG_VIDEO_CYCLIC_INTRA_REFRESH_MB` as macroblocks per frame
  derived from the picture size and the cycle) from the sys crate, set
  `GOP_SIZE` to zero for an unbounded GOP, and refuse the mode when the driver
  rejects both controls, unlike the self-disabling `keyframes` fallback used
  for forced keyframes.
- A cut in refresh mode returns an error from this backend: the shared
  contract says a cut restarts the sweep, and V4L2 cannot, so it refuses
  rather than opening a group out of phase with the refreshed macroblocks.
  The producer's forced cut on (re)open does not reach the backend, because a
  new encoder's first frame is a sweep start by construction; the producer
  treats it as the cut. The count stays aligned to the driver's period from
  that frame, and the backend reports the configured period as its sweep
  length.
- Verify on the hardware the backend already targets that the first sweep
  begins at frame zero and note whether the driver emits the SEI; the hang
  side does not need it.

## Required

- [Encode config](/quest/m2/intra-refresh/encode-config.md) - the `Gop` enum and cut semantics this implements
