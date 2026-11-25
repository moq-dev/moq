# [L] Encode Opus without unsafe code, and delete moq-libopus

## Goal

moq-audio's Opus encoder backend (float encode with bitrate, DTX, signal, and
application settings) runs on the picked path under
`#![forbid(unsafe_code)]` and passes the harness. `rs/moq-libopus` and its
workspace entries are deleted.

## Required

- [Decode Opus without unsafe code](/quest/m3/opus-safe/decoder.md) - settles the path and lands the decoder half
