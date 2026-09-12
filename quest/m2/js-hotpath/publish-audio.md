# [M] Stop opening one QUIC stream per Opus frame

## Goal

Audio publish stream-open rate drops from ~50/s to GOP-like. Lag and drift
are no worse than grouping. Loss is still concealed.

## Plan

`js/publish/src/audio/encoder.ts` writes each Opus frame as its own group
so "the relay can forward it without waiting for a group boundary." At
20 ms that is 50 unidirectional streams/s, competing with video GOPs for
WebTransport stream credit (`Writer.tryOpen(..., waitUntilAvailable: false)`).

Prefer datagrams when the session has them and the payload fits; otherwise
a short-lived group of N frames or one audio group stream. Preserve PLC in
the catalog.

This is an intentional grouping change, not a micro-opt. Measure before
committing to datagrams vs grouped streams.

Acceptance: browser publish to a local relay: stream-open rate,
`QuotaExceeded`/dropped groups, encoder input-to-output lag. Stream opens
drop from ~50/s. Lag/drift no worse than grouping.

## Related

- [Browser benchmarks](/quest/m2/browser-benchmarks.md) - publish scenario
