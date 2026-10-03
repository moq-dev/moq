# [L] Sans-IO IETF session

## Goal

The moq-transport session is driven like the [sans-IO lite session](/quest/m1/rs2ts/sans-io/lite.md):
bytes, stream events, and `tick(now)` in, bytes and model events out.

## Plan

Follow whatever shape the lite session settles on. The IETF code is the
largest module (about 12.7k non-test lines) and today compiles part of itself
twice (for `Session` and `ControlStreamAdapter<Session>`); collapse that while
here.
If the [async feature](/quest/m1/rs2ts/sans-io/async-feature.md) landed
first with the IETF session behind it, move the session out.

Public API: breaks moq-net's IETF session API. Wire: none.

Decided in the 2026-09-30 audit: deferred to m2 with generated IETF, its only
consumer, until generated lite passes its go/no-go.

## Required

- [Sans-IO lite session](/quest/m1/rs2ts/sans-io/lite.md) - sets the driver shape
