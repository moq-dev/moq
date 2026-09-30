# [XS] Demo serve-hls publishes distinct renditions

## Goal

`just pub serve-hls` serves two renditions at different resolutions, as
`just pub hls` does. Today both come out 256 wide: the `-vf:0` and `-vf:1`
options aren't per-stream in ffmpeg, so the last filter wins.

## Plan

Use per-stream filters (`-filter:v:0`, `-filter:v:1`), scaled to 720p and
144p to match `hls`. Scaling the 720p source up to 1080p couldn't encode in
real time on a dev machine. The recipe body moves to `sh/demo/serve-hls.sh`
when the [tooling line](/quest/m1/tooling/README.md) lands; fix it wherever it
lives. Check it by fetching `master.m3u8` and comparing the two renditions'
`RESOLUTION` attributes.
