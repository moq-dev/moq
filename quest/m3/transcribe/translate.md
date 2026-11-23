# [L] Translate a transcript into chosen languages

## Goal

`moq-transcribe` can be configured with target languages. Each becomes
another transcript rendition in the derivative catalog, translated from the
source's speech, on demand like the source transcript. The first runtime is
OpenAI's live translation.

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

`GPT-Realtime-Translate` takes audio, not text, and streams translated text
and audio while the source speaks, so the translator feeds it the decoded
source audio the transcriber hears, one session per target language, and
discards the translated audio. Each translation mirrors the source
transcript's groups, one per utterance, opening and finishing a group at the
utterance's bounds; the model's deltas become interim revisions and each
completed turn a final segment. A segment is timed on the source clock by the
audio it was fed, so it lags the speech by the model's delay, which watch
absorbs as it does any late text ([the format](/quest/m3/transcribe/format.md)).
Its text is the model's own translation of the speech, not of the source
transcript, so the two can word things differently.

## Waiting

- Maintainer: keep `GPT-Realtime-Translate`, found in review to take audio
  input only, or translate the source transcript's text instead? Options:
  (a) keep it, fed the source audio as above (recommended): translated text
  streams with the speech, about as late as the source transcript, and its
  translated audio could later become a dubbed audio rendition; it costs a
  hosted audio session per language ($0.034/min each), times segments only
  as closely as the model's delay, and can word things differently from the
  source transcript. (b) Translate each final source segment with a hosted
  text model: one translated segment per source segment, never interim
  text, but each lands a pause plus a model round trip after the speech. An
  agent cannot take this: it chooses the decided runtime. Done when the
  answer is in this Plan and this item is removed.

## Required

- [moq-transcribe with an OpenAI backend](/quest/m3/transcribe/crate.md) - the source transcript it translates
