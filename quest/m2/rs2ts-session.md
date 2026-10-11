# [L] Generated lite session

## Goal

@moq/net's lite session is generated from moq-net by rs2ts over the
generated model, and the hand-written js/net lite session is deleted.
Hand-written TypeScript remains only for the transport pumps, timers, and the
Promise helpers over the poll API. The translated session tests and
`just test interop --all` pass.

## Plan

Decided in the 2026-10-10 audit: the
[final browser report](/quest/m2/rs2ts-browser-report.md) follows this; do
not wait for it to land generated lite.

The session is generic over `transport::poll::Session` throughout (`Reader`,
`Writer`, `Driver`, `Publisher`); the sans-IO rewrite should remove most of
that before translation.

Public API: breaks `@moq/net`. Wire: none.

## Required

- [Sans-IO lite session](/quest/m2/rs2ts-sans-io-lite.md) - the session shape it translates
- [Generated model](/quest/m2/rs2ts-model.md) - the model it drives
