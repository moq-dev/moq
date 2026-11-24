# [L] moq-transcribe with an OpenAI backend

## Goal

A `moq-transcribe` crate consumes a hang broadcast's audio and publishes a
derivative broadcast whose catalog carries a transcript rendition. Nothing
is transcribed until someone subscribes, and the work stops after the last
subscriber leaves. A fetch or time start transcribes the span it asks for,
with a few seconds of preceding audio as context. The first backend is
OpenAI's `gpt-live-transcribe`, configured with the caller's API key.

## Plan

Follow `moq-transcode`'s structure for the derivative catalog and demand
tracking (`rs/moq-transcode/src/rung.rs`), including its session budget,
which here caps concurrent hosted sessions. Decode with `moq-audio` and
resample to what the backend takes.

`gpt-live-transcribe` streams text deltas but has no server-side turn
detection, so the crate detects speech itself. Speech onset opens an
utterance's group ([the format](/quest/m3/transcribe/format.md)), and the
silence that ends the utterance finishes it, however long it ran. Inside an
utterance, the crate also commits the backend's buffer at each shorter pause,
which ends a segment: its interim frames follow the deltas, and its final
frame lands when that commit's transcript completes. Committing at pauses
keeps a final within a segment of the speech even when an utterance runs on,
which is what lets an exporter write it
([export](/quest/m3/transcribe/export.md)). Measure the latency from speech to
the first interim and to the final, and record both in the crate's README.

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
committed and its group finished there, so the gap's words land in their own
group and the kept group stays as published. A word straddling that boundary
may appear in both; replacing published text to avoid that would break the
rule that a group's content never changes.

The transcript rendition is an ordinary catalog text rendition, listed
beside any captions the source already carries
([caption import](/quest/m3/transcribe/caption-import.md)). It takes
`role: "caption"` and is labeled as generated, so a player's menu tells it
apart from imported captions in the same language.

`gpt-live-transcribe` takes expected languages as a hint and returns no
detected language, so the crate detects it from the text: a local text
language identifier (such as `whatlang`) labels each final segment's `lang`,
and the rendition's catalog language is set once the finals agree, as the
catalog already changes mid-stream. The caller may name the expected
languages, which go to the backend as its hint. A backend that returns a
detected language, such as whisper, supplies it instead.

The backend interface takes audio in and returns transcript events, with no
callback parameters (the `AGENTS.md` API rules). Keep the interface private
until the second backend needs it.

Decided 2026-11-23 (maintainer, `/quest-run`): ✅ `role: "caption"` for the
source-language transcript (rejected: a new `transcript` role; `subtitle`
throughout).

Public API: a new crate. Wire: none beyond the format.

## Required

- [A structured transcript text format](/quest/m3/transcribe/format.md) - the rendition this publishes
