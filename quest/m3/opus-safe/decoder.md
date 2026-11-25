# [L] Decode Opus without unsafe code

## Goal

moq-audio's Opus decoder backend runs on the path the maintainer picks:
multistream float decode, PLC, gain, reset, and the packet parse helpers moq-audio
calls, all under `#![forbid(unsafe_code)]` and passing the harness's
correctness bar. moq-audio stops calling moq-libopus for decoding.

## Plan

Start only after the maintainer picks a path. The harness PR adds that
decision here as a `Maintainer:` item under `Waiting`.

Record what the picked path still lacks for encoding in
[the encoder quest](/quest/m3/opus-safe/encoder.md)'s Plan.

## Required

- [Compare safe Opus candidates](/quest/m3/opus-safe/harness.md) - the harness that gates this swap and the comparison the maintainer picks from
