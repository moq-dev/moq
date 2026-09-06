# [L] Back an internal OBS audio encoder with moq-audio

## Goal

MoQ publishing can encode OBS's mixed audio with moq-audio Opus while preserving OBS output timing. The adapter is internal to the MoQ integration, with the existing OBS encoder mode retained.

## Plan

- Wrap `moq_audio::encode::Encoder` through a codec-only C interface with owned encoder/packet handles. Existing `moq_publish_audio_raw` combines encoding and publication and must not create a second publication alongside the OBS encoded output. Reuse frame sizing, bitrate updates, catalog configuration, and finish/padding behavior.
- Implement the OBS audio encoder interface, including fixed input frame size, mono/stereo PCM conversion, timestamps, codec headers, final padding, and packet release. Ask OBS for the input layout the encoder supports; moq-audio does not implement arbitrary channel remapping. Keep capture/mixing/device ownership in OBS.
- Publish Opus initially. Leave AAC with the existing OBS mode and defer PCM publishing UI, since the output currently declares AAC/Opus. Correct stale C documentation that describes the raw codec parser as Opus-only if that API is touched.
- Apply the shared presets and independent audio bitrate. Test 10/20 ms packetization, frame-size changes at stream boundaries, partial final frames, silence, reconnect, saturation, and stop with pending output. Verify decoded audio and A/V timestamps with a real subscriber.
- Land the internal adapter independently; the combined Use MoQ encoders UI becomes available when the video adapter also lands. Do not expose a temporary video/audio mix-and-match product UI.

## Required

- [Encoder presets](/quest/m2/obs-moq-video/presets.md) - shared policy and truthful reporting
