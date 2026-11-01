# [S] moq-mux's AAC config reports backward-compatible SBR

## Goal

`moq_mux::codec::aac::Config` reports the backward-compatible SBR extension of an AudioSpecificConfig, so moq-audio reads it from there and stops walking the AudioSpecificConfig itself.

## Plan

Follow-up of [moq-dev/moq#7398](https://github.com/moq-dev/moq/pull/7398), which moved moq-audio's SBR check onto `bitstream-io` rather than add moq-mux API.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Add the field to moq-mux's public config and delete moq-audio's own parse; test an implicit and an explicit SBR config.

Public API: an additive field on `moq_mux::codec::aac::Config`. Wire: none.

## Required

- [Parse H.264 and codec config bits with maintained crates](/quest/m2/h264-reader.md) - its PR, #7398, rewrites both readers
