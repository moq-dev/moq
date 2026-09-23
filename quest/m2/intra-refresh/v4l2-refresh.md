# [S] V4L2 encodes refresh mode

## Goal

With `Gop::Refresh`, the V4L2 backend asks the hardware for periodic intra
refresh and no IDR after the first frame; a driver that lacks the control
refuses the mode. Groups come from the producer's frame count, so a driver
that emits no recovery-point SEI still forms one group per sweep. Periodic
refresh controls do not by themselves guarantee an immediate sweep restart.
Verify the selected driver; refuse an unsupported mid-cycle cut rather than
claiming a boundary that was not encoded.

## Plan

- `rs/moq-video/src/encode/backend/v4l2.rs`: import
  `V4L2_CID_MPEG_VIDEO_INTRA_REFRESH_PERIOD` (falling back to
  `V4L2_CID_MPEG_VIDEO_CYCLIC_INTRA_REFRESH_MB` as macroblocks per frame
  derived from the picture size and the cycle) from the sys crate, set
  `GOP_SIZE` to zero for an unbounded GOP, and refuse the mode when the driver
  rejects both controls, the same way a missing `FORCE_KEY_FRAME` control
  refuses cuts with `CutUnsupported` at open.
- A cut in refresh mode restarts the sweep. If the driver cannot honor it,
  report rejection through the error path settled in main; this does not
  independently require a Result from cut(). Never open a group out of phase
  with the refreshed macroblocks.
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
