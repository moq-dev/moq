# [L] Translate a transcript into chosen languages

## Goal

`moq-transcribe` can be configured with target languages. Each becomes
another transcript rendition in the derivative catalog, translated from the
source transcript's text segment by segment, on demand like the source
transcript. The first runtime is OpenAI's `gpt-realtime-2` in a text-only
Realtime session.

## Plan

Decided 2026-11-17 (maintainer, `/quest-plan`): ✅ one rendition per target
language (rejected: English-only translation; deferring translation). Why:
viewers pick a language the way they pick a rendition.

Decided 2026-11-23 (maintainer, `/quest-run`): ✅ a hosted OpenAI runtime
first (rejected: a local model first, such as an open multilingual LLM
through llama.cpp or NLLB through CTranslate2; defining the interface only).
A self-hosted runtime can follow as its own quest, as
[transcribe-local](/quest/m4/transcribe-local.md) does for transcription.

Decided 2026-11-23 (maintainer, `/quest-run`): ✅ `gpt-realtime-2` in a
text-only Realtime session (`wss://api.openai.com/v1/realtime?model=gpt-realtime-2`,
text in and text out), translating the transcript's text segment by segment
over one persistent session per target language, keeping context across
segments, and appending to stable partials so published text never changes
(rejected: `GPT-Realtime-Translate` fed the source audio, since it accepts
only audio and so can't translate the transcript; DeepL on final segments; a
local LLM continuing an append-only translation, the AlignAtt pattern; local
NMT such as NLLB or MADLAD). Pricing: $4 per million text tokens in, $24 per
million out.

A translation takes `role: "subtitle"`: the source-language transcript is the
`caption` ([crate](/quest/m3/transcribe/crate.md)), and a translation of
speech is a subtitle for viewers who hear the audio but not the language.

**One translated segment per source segment.** The translator reads the
transcript the crate itself produces, in process, so no remote frame reaches
the model. Each translation track mirrors the source transcript's groups: one
group per source utterance, at the same start. Each of its frames revises the
segment with the source segment's `id`, `start`, and `end`, and the target
`lang`, so a translated segment and its source map one to one and line up
with the same audio. It carries no `words` or `confidence`, since word timing
and scores don't carry across languages. A source segment already in the
target language is copied rather than sent to the model.

**Append-only revisions.** Each revision's text extends the previous one's,
and the last is marked final, so a viewer never sees translated text change
or vanish. Source interims follow the backend's deltas
([crate](/quest/m3/transcribe/crate.md)), so the translator treats a source
interim's text up to its last clause boundary as stable. It asks the model to
translate only the stable text past what it already translated, continuing
the segment's translation, and publishes the result appended as the next
interim. When the source's final arrives, the translator translates the rest
and publishes the final. If the backend's final rewrote text the translator
already translated, the translation keeps its published words and continues
from the last clause boundary the two share. A rewritten phrase may then read
twice; the rule that published text never changes accepts that, as the
crate accepts a word straddling a kept group. Measure how often finals rewrite
translated text, and record it in the crate's README.

**Late text in watch.** Translated text lands after the source text by the
model's round trip. Its times are the source segment's, so
[the format](/quest/m3/transcribe/format.md)'s rule for late text applies
unchanged. Watch shows a segment's newest revision as soon as it arrives,
even after its `start`, and keeps one that arrives after its `end` up for its
own duration from arrival. Each appended revision is a newer revision, so
one arriving after `end` restarts that window.

**Order and backlog.** One session per language translates segments in source
order, so a translation finishes an utterance's group, once its last segment's
final is out, before it opens the next. Its open group therefore stays the
newest, which max delay never skips, as the format requires. When the
translation falls behind the live source transcript by more than the track's
max age (30 s), it skips to the newest source group, so a stalled model costs
a gap, not an unbounded queue. A fetch or time start into a past span
translates the source transcript's kept groups there, with the segments
before it as context. Like the transcriber, the translator keeps each group
it published for as long as the source transcript keeps the group it
translated, and answers later requests from those.

**Session.** Open the session with `output_modalities: ["text"]`, the lowest
reasoning effort, and instructions to return only the continuation of the
translation into the target language. Send each stable span as a text
conversation item, and read the response's text deltas into the interim. The
session keeps recent source and translation pairs as conversation items, so
the model translates with context. Trim the oldest in batches, not one per
segment, so the input per response stays bounded and the cached prompt prefix
survives between trims. A Realtime session lasts at most 60 minutes, so open
its replacement before then, seeded with the recent pairs. Sessions count
toward the crate's session budget. Measure the latency from a source final to
its translated final, and the cost per hour of speech in each language, and
record both in the crate's README.

Public API: target languages in the crate's config. Wire: none beyond the
format.

## Required

- [moq-transcribe with an OpenAI backend](/quest/m3/transcribe/crate.md) - the source transcript it translates
