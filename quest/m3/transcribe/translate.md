# [L] Translate a transcript into chosen languages

## Goal

`moq-transcribe` can be configured with target languages. Each becomes
another transcript rendition in the derivative catalog, translated from the
source-language segments, on demand like the source transcript. The first
runtime is OpenAI's live translation.

## Plan

Decided 2026-11-17 (maintainer, `/quest-plan`): ✅ one rendition per target
language (rejected: English-only translation; deferring translation). Why:
viewers pick a language the way they pick a rendition.

Decided 2026-11-23 (maintainer, `/quest-run`): ✅ OpenAI's live translation
first (`GPT-Realtime-Translate`, about $0.034/min, hosted like the first
transcription backend) (rejected: a local model first, such as an open
multilingual LLM through llama.cpp or NLLB through CTranslate2; defining
the interface only). A self-hosted runtime can follow as its own quest, as
[transcribe-local](/quest/m4/transcribe-local.md) does for transcription.

A translation takes `role: "subtitle"`: the source-language transcript is the
`caption` ([crate](/quest/m3/transcribe/crate.md)), and a translation of
speech is a subtitle for viewers who hear the audio but not the language.

Each translation mirrors the source transcript's groups, one per utterance,
and its segments translate the source's final segments, so a translation
never shows interim text the source later revises.

## Required

- [moq-transcribe with an OpenAI backend](/quest/m3/transcribe/crate.md) - the source transcript it translates
