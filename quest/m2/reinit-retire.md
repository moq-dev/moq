# [S] fMP4 and MKV re-init retire old timelines

## Goal

When an fMP4 or MKV import re-initializes (a new init segment or EBML header
mid-stream), the old timeline is retired the way
[moq-dev/moq#6757](https://github.com/moq-dev/moq/pull/6757) retires an ended
TS route, so import state stays bounded across repeated re-inits.

## Plan

Reuse the retirement #6757 added to `rs/moq-mux/src/timeline.rs`. Test a
stream that re-inits many times and assert the import's state does not grow.

Public API: none. Wire: none.
