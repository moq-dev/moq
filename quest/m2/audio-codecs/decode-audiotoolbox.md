# [M] AudioToolbox decode on macOS and iOS

## Goal

On macOS and iOS, `moq-audio` decodes AAC-LC, HE-AAC v1 and v2, and
multichannel AAC through AudioToolbox, and whichever of AC-3, E-AC-3, MP3, and
FLAC the framework opens. The OBS source and `moq play` play the streams #3498
documents as unsupported.

## Plan

An `AudioConverter` from the packetized format to interleaved `f32` at the
codec's native rate and layout, behind the decode seam as the first platform
candidate on `target_os = "macos"` and `"ios"`. `objc2-audio-toolbox` is the
binding, alongside the `objc2-core-audio-types` the crate already carries.

- Build the `AudioStreamBasicDescription` and magic cookie from the catalog
  description; the converter reports the output layout, which maps to
  `Layout` from the AudioChannelLayout tag rather than a count.
- HE-AAC: the converter reads SBR in band and reports the doubled rate; the
  seam passes it through. No config-level guessing.
- Priming and remainder: AudioToolbox reports `kAudioConverterPrimeInfo`;
  trim it so timestamps line up with symphonia's output on the same stream.
- Every codec the backend advertises has a fixture and a decode test, and the
  test asserts the layout order matches the canonical one (the LFE and centre
  end up where `Layout` says).
- iOS: the binding compiles into the moq-ffi iOS slice; runtime proof waits
  on a device like the rest of the mobile line.
- Docs: `doc/bin/obs.md` drops the HE-AAC and multichannel caveat on macOS,
  and the backend table names what this host decodes.

## Required

- [Decode seam](/quest/m2/audio-codecs/decode-backend.md) - the candidate order this backend joins
- [Layout](/quest/m2/audio-codecs/layout.md) - what a multichannel frame is delivered as

## Related

- [Media Foundation decode](/quest/m2/audio-codecs/decode-mediafoundation.md) - the same shape on Windows
- [MediaCodec decode](/quest/m2/audio-codecs/decode-mediacodec.md) - the same shape on Android
