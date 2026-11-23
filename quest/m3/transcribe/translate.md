# [L] Translate a transcript into chosen languages

## Goal

`moq-transcribe` can be configured with target languages. Each becomes
another transcript rendition in the derivative catalog, translated from the
source-language segments, on demand like the source transcript.

## Plan

Decided 2026-11-17 (maintainer, `/quest-plan`): ✅ one rendition per target
language (rejected: English-only translation; deferring translation). Why:
viewers pick a language the way they pick a rendition.

Each translation's `role` follows the [crate](/quest/m3/transcribe/crate.md)'s
pick for transcripts.

## Required

- [moq-transcribe with an OpenAI backend](/quest/m3/transcribe/crate.md) - the source transcript it translates

## Waiting

- Maintainer: pick the translation runtime. Options: OpenAI's live translation (`GPT-Realtime-Translate`, about $0.034/min, hosted like the first transcription backend); an open multilingual LLM through llama.cpp (the same CUDA, Vulkan, Metal, and CPU reach as whisper.cpp); or NLLB through CTranslate2 (CUDA and CPU only, non-commercial license). Recommendation: OpenAI's live translation first, matching the hosted transcription backend, with llama.cpp as the self-hosted follow-up. An agent cannot weigh the provider spend.
