# [S] moq-audio's 5.1 PCM round trip reads its frame under load

## Goal

`moq-audio`'s `roundtrip::pcm_five_one_reads_back_at_any_layout` reads one
decoded frame per output layout on every run, instead of sometimes finding
the consumer ended first.

## Plan

Seen 2026-11-16 in a loaded `just check` on the branch of the hang-04
MPEG-TS passthrough change, which touches no audio code: `next_frame` returned
`None` at `rs/moq-audio/tests/integration/roundtrip.rs:239`, and three runs of
the test alone passed. The test finishes the producer right after creating the
consumers, so a consumer may see the end before its first frame. Reproduce
under load, find whether the decode consumer drops frames written before it
subscribed or the end overtakes them, and fix that cause.

Public API: none. Wire: none.
