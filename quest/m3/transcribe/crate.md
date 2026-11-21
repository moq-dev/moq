# [L] moq-transcribe with an OpenAI backend

## Goal

A `moq-transcribe` crate consumes a hang broadcast's audio and publishes a
derivative broadcast whose catalog carries a transcript rendition. Nothing
is transcribed until someone subscribes, and the work stops after the last
subscriber leaves. A fetch transcribes the span it asks for, with a few
seconds of preceding audio as context. The first backend is OpenAI's
`gpt-live-transcribe`, configured with the caller's API key.

## Plan

Follow `moq-transcode`'s structure for the derivative catalog, demand
tracking, and group mirroring. Decode with `moq-audio` and resample to what
the backend takes.

`gpt-live-transcribe` streams text deltas but has no server-side turn
detection, so the crate segments speech itself and commits each turn. A
segment's interim frames follow the deltas, and its final frame lands when
the turn's transcript completes. Measure the latency from speech to the first
interim and to the final, and record both in the crate's README.

The backend interface takes audio in and returns transcript events, with no
callback parameters (the `AGENTS.md` API rules). Keep the interface private
until the second backend needs it.

Public API: a new crate. Wire: none beyond the format.

## Required

- [A structured transcript text format](/quest/m3/transcribe/format.md) - the rendition this publishes
