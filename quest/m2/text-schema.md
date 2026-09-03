# [M] Text availability index

## Goal

A text track says what it actually covers. An independently hosted
transcription publishes its own local availability index instead of copying
the audio or video timeline it transcribes.

## Plan

The `text` catalog section already carries the rest of the contract: relative
`broadcast` references so a transcription can live in its own broadcast, cue
timing on the shared media clock, and `jitter` for the publisher's flush
delay. What it has no answer for is coverage.

A transcription contribution is not continuous and does not start when the
media does. It may join late, drop out, or cover only part of what it listens
to, so a consumer that reads the source's timeline learns nothing about which
spans actually have text. Copying that timeline would be worse than useless:
it would advertise coverage the transcriber never produced.

Give the text rendition its own availability index over the spans it has
published, on the same media clock as its cues, so a consumer can tell "no
speech here" from "no transcription here". Keep it local to the text track:
composing transcription contributions onto broadcasts stays downstream in
moq.pro.

Per cross-package sync, mirror the schema in `js/hang` and update
`drafts/draft-lcurley-moq-hang.md` in the same PR.
