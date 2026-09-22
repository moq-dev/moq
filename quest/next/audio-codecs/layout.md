# [M] Channel layouts through decode, playback, and the FFI

## Goal

`moq-audio` carries up to 7.1 end to end. A decoded frame says which layout
it is in, the playback mixer downmixes it to whatever the device opened, a raw
PCM consumer can ask for a layout, and the FFI and libmoq expose the same.
Proven with multichannel PCM, the one codec that needs no new decoder.

## Plan

Extend the Layout contract settled in main with supported surround layouts
(2.1, quad, 5.0, 5.1, 6.1, 7.1, and the other AAC channelConfiguration and
Opus mapping family 1 entries) in one canonical order, the SMPTE/WAVE order.
Keep the representation extensible. Each codec module maps its native
order into it: AAC's `C L R Ls Rs LFE` and Opus's Vorbis `L C R Ls Rs LFE`
both become `L R C LFE Ls Rs`. An unspecified discrete PCM layout remains
valid for passthrough but is refused for spatial remixing; never invent speaker
positions from an arbitrary channel count.

- Reuse the settled PCM descriptors and codec/consumer settings. This quest
  adds supported layout behavior rather than replacing public field types.
  Frame stays layout-free since the consumer fixed it at construction.
- `resample::remix` becomes a generic remix over layouts: ITU-R BS.775
  coefficients for downmix, silence in the extra speakers for upmix, and the
  existing mono/stereo paths as the two-channel special cases. The resampler is
  already channel-generic.
- Playback: the mix bus takes the device's layout instead of fixed stereo, the
  device chooser prefers the widest well-known layout the device offers (a
  5.1 HDMI sink opens at six channels, a headset at two), and each `Sink`
  remixes into the bus. Today's "silence past the front pair" fan-out goes.
- moq-ffi and libmoq keep `channels` as a count, and the count means the
  default layout for that count (the WAVE convention: 3 is 2.1, 4 is quad, 6
  is 5.1, 8 is 7.1), delivered or accepted in the canonical order. No record,
  `repr(C)` struct, or binding changes, so this stays on `main`; only the
  Rust API names the layout, and a binding that needs 5.0 rather than 5.1 is
  a later additive field. Document the mapping in every binding's audio doc.
- The catalog does not change: `channel_count` already carries what the
  description implies, and `js/hang`'s `aac.ts` stops falling back to stereo
  for a count it cannot map.
- Regressions: a 5.1 PCM broadcast published through `encode::Producer` and
  read back through `decode::Consumer` at 5.1, at stereo, and at mono; the
  mixer fed a 5.1 sink into a stereo bus and a stereo sink into a 5.1 bus; the
  device chooser picking six channels when offered.

Capture stays mono/stereo, and Opus encode stays mapping family 0.

## Related

- [Opus surround](/quest/next/audio-codecs/opus-surround.md) - the first coded multichannel consumer of the layout
- [AudioToolbox decode](/quest/next/audio-codecs/decode-audiotoolbox.md) - the first platform decoder producing more than stereo
