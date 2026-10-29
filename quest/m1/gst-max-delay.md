# [S] moqsrc takes a max-delay

## Goal

GStreamer's `moqsrc` has a `max-delay` element property (default 1 s, today's
hard-coded value) that sets every track's delay budget, and `interop.sh`
sets it to 5 s as the other interop readers allow.

## Plan

Decided 2026-10-27 (maintainer): a property, not a budget derived from the
track's max-age, since the budget is a latency tradeoff the viewer chooses
and publishers keep 30 s of max-age. Found by
[moq-dev/moq#6990](https://github.com/moq-dev/moq/pull/6990): a starved
GStreamer subscriber fails the gap check with no way to widen it
(`rs/moq-gst/src/source/imp.rs`). Update `doc/bin/gstreamer.md`.

Public API: a new `moqsrc` property. Wire: none.
