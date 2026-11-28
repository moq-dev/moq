# [L] moq-transcribe with a Deepgram backend

## Goal

A `moq-transcribe` crate consumes a hang broadcast's audio and publishes a
derivative broadcast whose catalog carries a transcript rendition. Nothing
is transcribed until someone subscribes, and the work stops after the last
subscriber leaves. A fetch or time start transcribes the span it asks for,
with a few seconds of preceding audio as context. The backend is Deepgram's
Nova-3 multilingual model, configured with the caller's API key.

## Plan

Follow `moq-transcode`'s structure for the derivative catalog and demand
tracking (`rs/moq-transcode/src/rung.rs`), including its session budget,
which here caps concurrent hosted sessions. Decode with `moq-audio` and
resample to what the backend takes.

Decided 2026-11-24 (maintainer, `/quest-plan`): ✅ Deepgram is the only
hosted backend (rejected: OpenAI `gpt-live-transcribe` first, then
Deepgram; OpenAI first as planned). Why: Deepgram's streaming API reports
interim and final results, utterance ends, word timing and confidence, and
each word's detected language, so the crate needs no speech detection or
text language identifier of its own. It is also about a third of the price
per minute.

Deepgram's endpointing drives the [format](/quest/m3/transcribe/format.md)'s
groups: speech onset (its `SpeechStarted` event, or the first result) opens
an utterance's group, each `is_final` result is a segment's final frame after
the interim frames that revised it, and its `UtteranceEnd` finishes the
group, however long the utterance ran. Deepgram reports times from the start
of the audio it was sent; map them onto the source clock through the
timestamps of the audio sent, so a transcript lines up with its source.
Measure the latency from speech to the first interim and to the final, and
record both in the crate's README.

Recommendation: stream to `/v1/listen` with `model=nova-3`, `language=multi`,
`interim_results`, `vad_events`, `utterance_end_ms`, and `mip_opt_out=true`,
so the provider retains no audio past the request (opting out has not
changed Deepgram's listed prices since 2026-03-05). Send KeepAlive while the
source carries no audio, since Deepgram closes an idle stream. Bound what a
response can make the crate hold: a segment's frame is one result, never an
accumulation.

Groups are keyed by the utterance's start, and where an utterance starts
depends on where the transcriber began listening, so a fresh run over a span
can cut it differently, and a hosted model never repeats its text exactly
either way. The transcriber is the publisher of these groups, not a cache of
them: it keeps each group it published for as long as the source holds the
audio beneath it, and answers a later request for that span from what it
kept. A time start into a span no run covered transcribes from a lead-in
before that time, and publishes the utterance containing it, or reports the
silence up to the next one, only within the gaps between kept groups, since
groups never overlap. A gap run treats the next kept group's start as the
silence that ends an utterance: speech running into a kept group is
finalized (Deepgram's `Finalize` message) and its group finished there, so
the gap's words land in their own group and the kept group stays as
published. A word straddling that boundary may appear in both; replacing
published text to avoid that would break the rule that a group's content
never changes. Recommendation: a gap run may use Deepgram's pre-recorded
endpoint instead of a stream, if its utterances map onto groups the same way
and it answers sooner.

The transcript rendition is an ordinary catalog text rendition, listed
beside any captions the source already carries
([caption import](/quest/m3/transcribe/caption-import.md)). It takes
`role: "caption"` and is labeled as generated, so a player's menu tells it
apart from imported captions in the same language.

Each final segment's `lang` is the language most of its words carry, from
Deepgram's per-word detection, and the rendition's catalog language is set
once the finals agree, as the catalog already changes mid-stream. The caller
may name the expected languages, which go to Deepgram as its hint.

The backend interface takes audio in and returns transcript events, with no
callback parameters (the `AGENTS.md` API rules). Keep the interface private
until [the local backend](/quest/m4/transcribe-local.md) needs it.

Decided 2026-11-23 (maintainer, `/quest-run`): ✅ `role: "caption"` for the
source-language transcript (rejected: a new `transcript` role; `subtitle`
throughout).

Public API: a new crate. Wire: none beyond the format.

## Required

- [A structured transcript text format](/quest/m3/transcribe/format.md) - the rendition this publishes
