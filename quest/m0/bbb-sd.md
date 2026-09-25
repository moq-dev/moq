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
  like the other assets. Add the encode as a `demo/pub` recipe so it is
  reproducible, then `just upload bbb-sd.mp4`; `bbb.mp4` stays unchanged for
  its other consumers.
- `bbb` downloads both and feeds them as two `-stream_loop -1 -re` inputs to
  one ffmpeg with `-map 0 -map 1:v -c copy` into `import ts`, which turns each
  video PID into its own rendition. Map the 720p stream first: WHEP and
  non-multitrack RTMP still serve the first rendition by name until
  [egress rendition pick](/quest/m1/egress-rendition-pick.md) lands.
- Verify: the catalog lists both renditions with a `bitrate`; the player
  switches down under throttling and back up; the two renditions stay
  timestamp-aligned after several loops (two looped inputs drift if their
  durations differ).

## Related

- [moq.pro demo simulcast](https://github.com/moq-dev/moq.pro/blob/main/quest/m0/demo-simulcast.md) - the always-on fleet demo switches to this asset
