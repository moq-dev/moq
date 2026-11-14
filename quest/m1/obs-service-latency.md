# [S] The MoQ service tunes every encoder for low latency

## Goal

OBS's own Start Streaming (or obs-websocket's `StartStream`) on `moq_service`
publishes with no B-frames or lookahead and a 2 s keyframe interval on every
encoder the dock's Go Live tunes (x264, NVENC, Quick Sync, VideoToolbox), so
`@moq/obs` stops reporting most encoders as keeping their defaults.

## Plan

Found by [moq-dev/moq#8721](https://github.com/moq-dev/moq/pull/8721): over
obs-websocket a profile can tune only x264 (`SimpleOutput.x264Settings`).
NVENC, the default encoder wherever it is available, keeps two B-frames and
an automatic keyframe interval (250 frames, about 8 s of group at 30 fps),
which delays every viewer's join; `Obs.configure` can only name it in
`ignored`.

- `MoQService::ApplyEncoderSettings` (`cpp/obs/src/moq-service.cpp`) sets only
  `repeat_headers`. OBS calls it on every start in Simple output mode, and in
  Advanced mode while "Apply service settings" stays on (the default), with
  the stream encoder's settings that the WHIP simulcast layers then copy.
  Apply `ApplyEncoderLatency` (`moq-encoder-latency.h`) there, plus
  `keyint_sec = 2`. It receives settings, not the encoder id, so key the
  tuning on the profile's encoder through the settings keys each encoder
  reads (`bf`, `bframes`, `x264opts`, `latency`), or pass the id if the API
  allows. Recommended: set every encoder's keys; each ignores the rest.
- Recommended: tune by default with no opt-out on the service, since a MoQ
  group starts at a keyframe and an 8 s group is never what a viewer wants.
  The dock keeps its own "Keep encoder settings" choice for Go Live.
- Then drop `keyint` and `bframes` from `@moq/obs`'s `ignored` for the
  encoders the service now tunes, and the x264 profile options it writes.
- Cover it with `just obs test`, and a live run on OBS 32.2.2 that checks the
  encoder log lines (`keyint`, `bf`) for x264 and one hardware encoder.

Public API: none. Wire: none.

## Hosts

- linux - the live run drives OBS with the plugin
