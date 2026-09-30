# [S] Audio presets mirror video, and the preset claims hold

## Goal

The encoder presets from [#4099](https://github.com/moq-dev/moq/pull/4099)
read the same in moq-video and moq-audio, and nothing they promise is false.
The shape is settled; this finishes it:

- Audio mirrors video: `moq_audio::encode::Settings` stores the preset it was
  given and reads it back, and the audio encoder reports an `Applied` the way
  `moq_video::encode::Encoder::applied` does. Today audio only has
  `Settings::with_preset`, which rewrites `frame_duration` and forgets the
  preset.
- The audio default agrees with itself: `Preset::default()` is `LowLatency`
  (10 ms), but `Settings::new()` builds 20 ms, which is `Balanced`.
- The `Preset` doc in `rs/moq-video/src/encode/encoder.rs` no longer claims
  that no preset reorders frames: V4L2 leaves reordering and queue depth to
  the driver and MediaCodec's no-B-frame setting is an unconfirmed hint. Only
  `Applied::preset` confirms it.
- `rs/moq-video/examples/encode-presets.rs` scores PSNR against the right
  source frames after the encoder skips some (it ignores the `.skipped`
  file today), and refuses an unknown preset name instead of printing the
  header and exiting 0.

## Plan

- Decided: `encode::Preset` and `Applied { preset: Option<Preset>, controls:
  String }` are the API; don't reopen them. Audio already has its `Preset`
  and gains the same `Applied` under `moq_audio::encode`.
- The audio enum default follows `Settings::new()`, not the other way
  round: its 20 ms is published in moq-audio 0.1.6 and matches the JS
  publish path, so `Preset::default()` becomes `Balanced` for audio. OBS
  defaults to Balanced as well (decided with the maintainer), so the plugin
  rides the published default instead of overriding it.
- Fail loud in the example: an unknown name is an error listing the valid
  ones.
- The line PR ([OBS native codecs](/quest/m1/obs-moq-video/README.md)) must
  call out the video default changes #4099 made: NVENC P4 to P1, and
  openh264 medium to low complexity.
- Known gap: VAAPI reports `LowLatency` whatever was asked, and V4L2 and
  MediaCodec report unconfirmed; no quest owns measuring and mapping presets
  for them. Media Foundation and VideoToolbox are owned by
  [Windows GPU input](/quest/m2/obs-windows.md) and
  [macOS GPU input](/quest/m2/obs-macos.md).

Public API: additive on moq-audio (the stored preset and its `Applied`
report); the unpublished audio `Preset` default changes. Wire: none.
