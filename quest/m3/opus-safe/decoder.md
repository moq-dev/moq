# [L] Decode Opus without unsafe code

## Goal

`rs/moq-opus`'s decode path runs on the path the maintainer picks:
multistream float decode, PLC, gain, reset, and the packet parse helpers moq-audio
calls, all in modules under `#![forbid(unsafe_code)]` and passing the harness's
correctness bar. moq-audio decodes through that safe API, with no `unsafe`
block of its own.

## Plan

Start only after the maintainer picks a path. The harness PR adds that
decision here as a `Maintainer:` item under `Waiting`.

Record what the picked path still lacks for encoding in
[the encoder quest](/quest/m3/opus-safe/encoder.md)'s Plan.

## Required

- [Compare safe Opus candidates](/quest/m3/opus-safe/harness.md) - the harness that gates this swap and the comparison the maintainer picks from
