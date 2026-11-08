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
  platform code (`just rs windows`).

Changed 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): [audio-codec-seam](/quest/m2/audio-codec-seam.md) makes moq-audio's codecs return zero or more outputs with `flush` before the cut, so this backend adds no break.

## Related

- [Windows decoded frames](/quest/m5/obs-decode-windows.md) - the OBS Windows line this feeds
