# [M] Implement refresh-mode groups on the settled GOP contract

## Goal

`moq_video::encode::Config` expresses the group structure as one enum, so a
caller picks keyframes at an interval or intra refresh with a cycle length and
cannot ask for both. A forced cut starts a new group in either mode: an IDR
with keyframes, a fresh sweep with refresh. In refresh mode the producer opens
a group at every sweep start and publishes `warmup` as the actual sweep
duration, which can be shorter than the cycle. Every
backend that cannot encode refresh mode refuses it when configured, and the
CLI and transcoder expose the choice. This extends the settled main contract
without replacing an API after 0.1.

## Plan

- Extend the non-exhaustive `Gop` contract from main with refresh mode. Keep
  the settled frame-count units and `cut()` operation; do not replace the
  public config or rename the operation again. A cut in refresh mode asks
  the backend to restart the sweep, including the producer's forced cut on
  every reopen.
- `Backend::encode(frame, cut)` in `rs/moq-video/src/encode/backend/mod.rs`
  keeps its flag, and each backend gets the mode at construction.
  VideoToolbox, openh264, VAAPI, Media Foundation, and MediaCodec return an
  error for `Refresh` (supported or refused, never a silent fallback to
  keyframes). The test-only probe backend accepts it so the producer logic is
  testable without hardware.
- Group boundaries in refresh mode come from counting: backends report
  `keyframe = false` for a sweep start, so the producer marks the first frame
  of each cycle by frame count from the last cut and opens the group there.
  A backend reports the sweep length it actually configured, which can be
  shorter than the cycle (NVENC needs it strictly shorter), and the producer
  publishes `warmup` as that length over the framerate.
- `rs/moq-transcode/src/rung.rs` keeps its eight-second override for
  `Keyframe` only; `Refresh` keeps the caller's cycle, defaulting to two
  seconds, since a cycle is both the tune-in delay and how thinly the intra
  bits are spread. `rs/moq-cli` `publish` and `transcode` expose the mode and
  interval, and `doc/bin/cli.md` documents it.
- Tests: the probe backend in refresh mode yields one group per cycle with
  `warmup` in the catalog; a cut mid-cycle opens a group and restarts the
  count; a backend without refresh support refuses the config.

## Required

- [Catalog warmup](/quest/next/catalog-warmup.md) - the field the producer publishes
