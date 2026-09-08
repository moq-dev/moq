# [M] Media Foundation decode on Windows

## Goal

On Windows, `moq-audio` decodes AAC-LC, HE-AAC v1 and v2, and multichannel
AAC through the Media Foundation AAC decoder MFT, and whichever of MP3, FLAC,
AC-3, and E-AC-3 the installed MFTs open.

## Plan

The audio counterpart of `rs/moq-video/src/decode/backend/mediafoundation.rs`,
using the `windows` crate features moq-video already enables plus the audio
ones. Behind the decode seam as the first candidate on `target_os =
"windows"`.

- The AAC decoder MFT takes an `MF_MT_USER_DATA` blob built from the catalog
  AudioSpecificConfig and reports the output `WAVEFORMATEXTENSIBLE`, whose
  channel mask maps to `Layout`.
- Optional MFTs (Dolby, FLAC) are probed at open: absent means the codec is
  not advertised on that host, not an error at decode time.
- Fixtures and layout-order tests as in the AudioToolbox quest.
- Verification runs on a Windows host; the per-PR CI only compiles the
  platform code, and `just rs windows` runs nightly.

## Required

- [Decode seam](/quest/m2/audio-codecs/decode-backend.md) - the candidate order this backend joins
- [Layout](/quest/m2/audio-codecs/layout.md) - what a multichannel frame is delivered as

## Related

- [Runtime QA hosts](/quest/m2/runtime-qa-hosts.md) - where the Windows run happens
- [Windows decoded frames](/quest/m2/obs-moq-video/decode-windows.md) - the OBS Windows line this feeds
