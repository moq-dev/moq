# Audio codecs

## Goal

A broadcast that plays in the browser plays natively. Today the browser
decodes whatever the platform offers (HE-AAC, 5.1 AAC, AC-3, surround Opus)
while `moq-audio` decodes Opus and AAC-LC in mono or stereo, so the same
stream plays on moq.dev and fails in OBS, `moq play`, and every binding. The
native side grows the backend seam `moq-video` already has, uses the OS codec
where one exists, keeps symphonia as the pure-Rust fallback, and carries more
than two channels end to end. Encoding follows the same seam so a native
publisher can produce AAC.

## Plan

Platform first, exactly like video: AudioToolbox on macOS and iOS, Media
Foundation on Windows, MediaCodec on Android, and symphonia (AAC-LC
mono/stereo) as the software fallback that openh264 is for H.264. Linux has no
OS audio decoder, so HE-AAC and multichannel AAC stay refused there, stated in
the docs and rejected at construction. A platform backend claims every catalog
codec its framework opens, so AC-3, E-AC-3, MP3, and FLAC ride along on the
hosts that have them; each still needs a fixture before the backend advertises
it.

Channels are a `Layout`, not a count: a closed set of well-known layouts in one
canonical order, derived from the codec description (AAC channelConfiguration,
OpusHead mapping) so the catalog and wire do not change. Every decoder reorders
from its codec's native order into that one, so the mixer, the FFI, and OBS
never guess where the LFE is. Playback downmixes to whatever the output device
opened. Counts with no standard layout are refused.

Encode mirrors decode: an `encode::backend` seam, `Codec::Aac` meaning AAC-LC
at the input's layout, and platform encoders behind it. Opus encode stays
mono/stereo.

The layout and seam quests are independent and come first; each platform then
lands as its own decode and encode quest so verification stays per host. The
HE-AAC refusal and the PCE parse are defects in what ships today and are
ready now.

## Quests

- [HE-AAC refusal](/quest/m2/audio-codecs/he-aac-refusal.md) - implicit-SBR HE-AAC over TS is refused instead of half-decoded as the LC core
- [AAC PCE](/quest/m2/audio-codecs/aac-pce.md) - a channel_config of 0 parses the program config element instead of guessing stereo
- [Layout](/quest/m2/audio-codecs/layout.md) - a `Layout` type in one canonical order carries up to 7.1 through decode, resample, playback, and the FFI
- [Decode seam](/quest/m2/audio-codecs/decode-backend.md) - `decode::backend` selects a platform decoder before symphonia, mirroring moq-video
- [AudioToolbox decode](/quest/m2/audio-codecs/decode-audiotoolbox.md) - macOS and iOS decode HE-AAC, multichannel AAC, and what else the framework offers
- [Opus surround](/quest/m2/audio-codecs/opus-surround.md) - mapping family 1 decodes on every host through the multistream decoder
- [Encode seam](/quest/m2/audio-codecs/encode-backend.md) - `encode::backend` and `Codec::Aac`, so a native publisher can produce AAC-LC
- [AudioToolbox encode](/quest/m2/audio-codecs/encode-audiotoolbox.md) - macOS and iOS encode AAC-LC
- [Media Foundation decode](/quest/m2/audio-codecs/decode-mediafoundation.md) - Windows decodes HE-AAC, multichannel AAC, and what else the MFTs offer
- [Media Foundation encode](/quest/m2/audio-codecs/encode-mediafoundation.md) - Windows encodes AAC-LC
- [MediaCodec decode](/quest/m2/audio-codecs/decode-mediacodec.md) - Android decodes HE-AAC, multichannel AAC, and what else the device offers
- [MediaCodec encode](/quest/m2/audio-codecs/encode-mediacodec.md) - Android encodes AAC-LC

## Related

- [OBS native codecs](/quest/m2/obs-moq-video/README.md) - the OBS source and encoder adapters consume this through libmoq; #3498 narrowed OBS to what moq-audio decodes today
- [Mobile](/quest/m2/mobile/README.md) - the iOS and Android slices ship these backends through moq-ffi
- [Runtime QA hosts](/quest/m2/runtime-qa-hosts.md) - Windows and Android verification needs a host; the Windows and macOS CI gates run nightly, not per PR
- [Dart codec parity](/quest/m2/dart-codecs.md) - Dart gains these once it builds with the `audio` feature
