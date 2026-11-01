# [S] Browser decoders join intra-refresh H.264 in every engine

## Goal

A js/watch viewer of an H.264 intra-refresh rendition (catalog `warmup` set)
joins and recovers from skips in Chromium, Firefox, and Safari. Each of its two
decoders (the min-delay and max-delay pair in `js/watch/src/video/decoder.ts`)
can open on a group's first frame, a recovery point that is not an IDR, and
decodes on from there, showing stripes until the sweep completes.

## Plan

The two decoders, their selector, and their tests landed with this quest's
first PR ([moq-dev/moq#7555](https://github.com/moq-dev/moq/pull/7555)).
What remains is getting the opening chunk past each engine's key check.

- Chromium (verified on 156): WebCodecs accepts an H.264 chunk sent as `key`
  only when it is an IDR or carries an SEI recovery point with
  `recovery_frame_cnt == 0` (`media/formats/mp4/avc.cc`, `AnalyzeAnnexB`). An
  intra-refresh recovery point counts a whole sweep, so `decode()` throws
  "A key frame is required" and the rendition fails through `error`. Adding a
  second SEI recovery point with a zero count ahead of the first slice of that
  one chunk makes Chromium accept it, and it outputs from that frame on.
  Open question for the maintainer: how js/watch handles this (see the PR's
  `## Decisions needed`).
- Firefox and Safari: unverified. Playwright's Firefox build reports H.264
  unsupported and its WebKit build would not launch on the Linux host. Check
  both with a real browser, joining at a non-IDR recovery point sent as `key`.
- HEVC: Chromium rejects a non-IRAP H.265 frame sent as `key`, so an HEVC
  refresh rendition fails through the decoder's existing `error` signal.

## Related

- [H.264 import](/quest/m2/intra-refresh/h264-import.md) - publishes the real `warmup` for an end-to-end x264 check
- [Consumer warmup](/quest/m2/intra-refresh/consumer-warmup.md) - the transcoder decodes history so it never encodes stripes
