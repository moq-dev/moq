# [XS] The OBS source joins audio at the live edge

## Goal

The OBS MoQ source's audio decoder starts at the newest cached group instead
of replaying the track's backlog, so a reconnect or rendition change plays
live audio at once.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7288](https://github.com/moq-dev/moq/pull/7288), which added
`DecodeStart` to the UniFFI decoders and left the plugin on the default:

- OBS plugin decode start:
  - Keep the default (oldest).
  - ✅ Switch to `DecodeStart::Latest`. Reason: the source is a live player,
    so replaying the cache only adds latency.

- `cpp/obs/src/moq-source.cpp:1081` (as of a86433043): set `output.start` to
  `moq::DecodeStart::kLatest` on the `moq::AudioDecoderOutput`. Video reads
  raw frames through `MediaTrackConsumer` and its own decoder, so it is
  unaffected.
- If cheap, an e2e check that an audio decoder with `Latest` starts at the
  newest group, beside [moq-dev/moq#7288](https://github.com/moq-dev/moq/pull/7288)'s `video_decode_start`.

The Go half of the original quest, deleting the wrapper's
`AudioDecoderOutput`/`VideoDecoderOutput` aliases, landed in
[moq-dev/moq#8303](https://github.com/moq-dev/moq/pull/8303).

Public API: none. Wire: none.
