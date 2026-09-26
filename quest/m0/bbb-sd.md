# [S] SD rendition for the bbb demo

## Goal

`just pub bbb` publishes `bbb.hang` with two video renditions, the 720p
source and a 360p ~600 kbps rung, and a player watching it through `just dev`
(including moq.dev's pages on localhost) switches between them on bandwidth
and viewport size. The SD rung is pre-encoded, so publishing costs no encode
CPU. moq.pro's always-on demo consumes the same asset.

## Plan

- Encode `bbb-sd.mp4`: video only, H.264 360p ~600 kbps, from `bbb.mp4` with
  identical frame count and timestamps, and keyframes forced at the source's
  keyframe times so both renditions switch on the same boundaries. Fragment it
  like the other assets. `just pub encode-bbb-sd` now reproduces the uploaded
  asset; `bbb.mp4` stays unchanged. The source has 14,313 visible frames and
  two duplicate packets marked discard. Preserve the visible frames, and
  extend the final SD sample duration to match the source audio's loop period:
  otherwise independent inputs drift by about 39 ms per loop.
- `bbb` downloads both and feeds them as two `-stream_loop -1 -re` inputs to
  one ffmpeg with `-map 0 -map 1:v -c copy` into `import ts`, which turns each
  video PID into its own rendition. Map the 720p stream first: WHEP and
  non-multitrack RTMP still serve the first rendition by name until
  [egress rendition pick](/quest/m1/egress-rendition-pick.md) lands.
- Verified: both catalog renditions have a `bitrate`, HD comes first, and
  `just pub check-bbb` compares all visible timestamps and keyframes across
  three loops. Nightly CI runs this check against the hosted assets.
- Chromium with `just dev` switches down and back up on viewport changes and
  explicit bitrate caps. A local UDP proxy capped at 1.3 Mbps also produced
  rendered 720p -> 360p -> 720p transitions, with reported receive estimates
  of about 1.63 Mbps while capped and 23.3 Mbps after recovery.
- Remaining: investigate an earlier throttle run that kept HD selected and
  skipped groups for the 45-second observation window. The cause is not yet
  established; the successful instrumented run does not explain it. Verify
  moq.dev's localhost pages and the related moq.pro fleet integration.

## Related

- [moq.pro demo simulcast](https://github.com/moq-dev/moq.pro/blob/main/quest/m0/demo-simulcast.md) - the always-on fleet demo switches to this asset
