# [S] An idle TS export with a delay costs what one without does

## Goal

An idle MPEG-TS export poll with a delay costs about what one without a delay
does, about 1.4k instructions per track. Today it is about 3k.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Found by [moq-dev/moq#7504](https://github.com/moq-dev/moq/pull/7504). With
a delay, `jitter::Acquire::freshest` (18%) and `memcmp` (26%) lead the idle
poll (`rs/moq-mux`). Skip the freshest scan when nothing arrived, and find what
the memcmp compares on an idle poll. Measure with #7504's export idle bench.
