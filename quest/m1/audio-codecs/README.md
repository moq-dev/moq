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
OS audio decoder, so multichannel AAC stays refused there, stated in the docs
and rejected at construction. HE-AAC signaled only in band (implicit SBR, as
over MPEG-TS) plays as its half-rate LC core on symphonia; detecting it needs a
full element walk, so the docs state it instead of refusing it. A platform backend claims every catalog
codec its framework opens, so AC-3, E-AC-3, MP3, and FLAC ride along on the
hosts that have them; each still needs a fixture before the backend advertises
it.

Use the extensible `Layout` contract settled in main, with well-known layouts in one
canonical order, derived from the codec description (AAC channelConfiguration,
OpusHead mapping) so the catalog and wire do not change. Every decoder reorders
from its codec's native order into that one, so the mixer, the FFI, and OBS
never guess where the LFE is. Playback downmixes to whatever the output device
opened. Preserve the existing arbitrary-channel PCM passthrough through an
unspecified discrete layout; refuse spatial remixing when speaker positions
are unknown instead of guessing them.

Encode mirrors decode: an `encode::backend` seam, `Codec::Aac` meaning AAC-LC
at the input's layout, and platform encoders behind it. Opus encode stays
mono/stereo.

The core configuration and layout contracts land in main. These quests implement
surround and backend dispatch on that contract; each platform then lands as
its own decode and encode quest so verification stays per host.

## Quests

- [AudioToolbox decode](/quest/m1/audio-codecs/decode-audiotoolbox.md) - macOS and iOS decode HE-AAC, multichannel AAC, and what else the framework offers
- [Opus surround](/quest/m1/audio-codecs/opus-surround.md) - mapping family 1 decodes on every host through the multistream decoder
- [ADTS refusals](/quest/m1/audio-codecs/adts-refusals.md) - the ADTS writer refuses channel counts and object types it cannot label instead of mislabeling them
- [AudioToolbox encode](/quest/m1/audio-codecs/encode-audiotoolbox.md) - macOS and iOS encode AAC-LC

## Related

- [OBS native codecs](/quest/m1/obs-moq-video/README.md) - the OBS source and encoder adapters consume this through moq-ffi; #3498 narrowed OBS to what moq-audio decodes today
- [Runtime QA hosts](/quest/m2/runtime-qa-hosts.md) - Windows and Android verification needs a host; the Windows and macOS CI gates run nightly, not per PR
- [Dart codec parity](/quest/m1/dart-codecs.md) - Dart gains these once it builds with the `audio` feature
- [Media Foundation decode](/quest/m2/audio-decode-mediafoundation.md) - Windows decodes HE-AAC, multichannel AAC, and what else the MFTs offer
- [Media Foundation encode](/quest/m2/audio-encode-mediafoundation.md) - Windows encodes AAC-LC
- [MediaCodec decode](/quest/m2/audio-decode-mediacodec.md) - Android decodes HE-AAC, multichannel AAC, and what else the device offers
- [MediaCodec encode](/quest/m2/audio-encode-mediacodec.md) - Android encodes AAC-LC
