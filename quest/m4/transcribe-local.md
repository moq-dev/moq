# [M] Transcribe on local hardware

## Goal

`moq-transcribe` gains a self-hosted backend that runs with no hosted API, on
NVIDIA, AMD and Intel GPUs (Vulkan), Apple GPUs (Metal), and CPU, choosing the
backend at setup from what the host has. The caller supplies the model
weights.

## Plan

Decided 2026-11-17 (maintainer, `/quest-plan`): ✅ NVIDIA, Vulkan, Metal, and
CPU (rejected: NVIDIA and CPU only). Why: transcription should run wherever
`moq-transcode` does.

## Required

- [moq-transcribe with an OpenAI backend](/quest/m3/transcribe/crate.md) - the crate and backend interface this adds to

## Waiting

- Maintainer: pick the engine. Options: whisper.cpp through `whisper-rs`, the only candidate covering all four backends; NVIDIA Parakeet or Canary through ONNX Runtime, which has no Vulkan; or candle, pure Rust with no Vulkan. Recommendation: whisper.cpp, since the hardware rule leaves no other candidate. The maintainer left the engine open.
