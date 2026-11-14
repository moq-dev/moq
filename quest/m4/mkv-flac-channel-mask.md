# [S] MKV imports ffmpeg's multichannel FLAC

## Goal

An MKV whose FLAC `CodecPrivate` holds STREAMINFO plus a Vorbis comment
carrying only `WAVEFORMATEXTENSIBLE_CHANNEL_MASK` imports, so ffmpeg's default
`-c:a flac` multichannel output (its default 6-channel 5.1, 2.1, 4.0, 5.0 and
7.1(wide)) needs no remux. Every other metadata block stays refused, and the
comment is bounded.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run), from [moq-dev/moq#7629](https://github.com/moq-dev/moq/pull/7629), which
bounds FLAC's `CodecPrivate` at STREAMINFO and documents the remux. Bound the
vendor string and the single field by their syntax, not by a sampled constant.
