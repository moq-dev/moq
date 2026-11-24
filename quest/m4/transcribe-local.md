# [M] Transcribe on local hardware

## Goal

`moq-transcribe` gains a self-hosted backend built on whisper.cpp through
`whisper-rs`, which runs with no hosted API, on NVIDIA, AMD and Intel GPUs
(Vulkan), Apple GPUs (Metal), and CPU, choosing the backend at setup from what
the host has. The caller supplies the model weights.

## Plan

Decided 2026-11-17 (maintainer, `/quest-plan`): ✅ NVIDIA, Vulkan, Metal, and
CPU (rejected: NVIDIA and CPU only). Why: transcription should run wherever
`moq-transcode` does.

Decided 2026-11-23 (maintainer, `/quest-run`): ✅ whisper.cpp through
`whisper-rs` (rejected: candle Whisper, pure Rust with no Vulkan; sherpa-onnx,
through ONNX Runtime, which has no Vulkan). Why: the only engine covering all
four backends.

Whisper decodes windows of audio rather than streaming, so the backend feeds
it each segment the crate's pause detection commits and returns the result as
that segment's final, with an interim from a decode of the segment so far
where the hardware keeps up.

## Required

- [moq-transcribe with an OpenAI backend](/quest/m3/transcribe/crate.md) - the crate and backend interface this adds to
