# [S] A restarted TS import publishes its catalog at once

## Goal

When `moq import ts/srt` republishes after a timebase step, the new epoch's
catalog goes out with its first media. It no longer waits ~2 s for a fresh
mux-rate window, so subscribers lose only the splice, not the catalog hold.

## Plan

Measured in [#7102](https://github.com/moq-dev/moq/issues/7102): a supervised
restart cost a subscriber 2.8 s. 1.2 s was the old instance's tail, which
[moq-dev/moq#7049](https://github.com/moq-dev/moq/pull/7049) dropped. 1.6 s was
the restarted import holding its catalog for its first mux-rate window.

Decided 2026-10-28 (maintainer): scoped to supervised restarts. The new
instance reuses the previous instance's mux-rate measurement, which describes
the same multiplex, and refines it as usual. Catalog publication stays coupled
to the measurement for a first start, so the epic's out-of-scope note stands.

Tests: replaying a step through the supervisor, the second epoch's catalog
is published with its first group.

Public API: none. Wire: none.
