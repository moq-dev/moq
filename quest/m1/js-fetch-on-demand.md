# [S] Serve on-demand IETF FETCH in JavaScript

## Goal

A JavaScript publisher answers IETF FETCH from a native subscriber for groups
that are no longer in its live cache, or never were, by dispatching onto the
on-demand request surface `@moq/net` gains in
[JS ranges](/quest/m1/subscribe-ranges/js.md).

## Plan

Decided 2026-10-27 (maintainer): split out of
[cached JavaScript FETCH](/quest/m1/js-fetch.md), which starts now, so only
this half waits on JS ranges. Decided in the 2026-10-05 audit: the
producer-side request surface folded into JS ranges, so it takes range
requests from the start, and this half only dispatches IETF FETCH onto it.

Verify with an in-memory application responder: a browser publisher serves a
native IETF subscriber after a group is evicted or was never cached. Run the
supported-draft matrix and `just test interop --all` through CI.

Public API: none beyond JS ranges' surface. Wire: none beyond the cached
half's FETCH support.

## Required

- [JS ranges](/quest/m1/subscribe-ranges/js.md) - the on-demand request surface this dispatches onto
- [Cached JavaScript FETCH](/quest/m1/js-fetch.md) - the IETF FETCH dispatch and codecs this extends
