# [S] A JS reader whose range ends below the cache finishes after a clean close

## Goal

In `@moq/net`, after a track closes cleanly, a reader whose range ends below groups the cache still holds receives those groups and then finishes, as in Rust, instead of waiting forever for the range to widen.

## Plan

Found by the agent on [moq-dev/moq#7349](https://github.com/moq-dev/moq/pull/7349) while writing its tests; the bug is already on main.

Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Reproduce with a test in `js/net` first, then fix the reader's end condition: once the track is closed, a range that can no longer grow is complete when every group in it is delivered or known missing. Check Rust's equivalent reader with the same scenario and add the test there too if it lacks one.

Public API: none. Wire: none.
