# [S] moq-quic collapses padding runs

## Goal

moq-quic's frame iterator yields one frame for a run of PADDING bytes instead
of one per byte, cutting the roughly 30% of debug-build handshake CPU it
costs today, and the padding stats still count what they mean to count.

## Plan

Found by [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987)'s
profile of the slowest tests. First check what the padding stats should
count (bytes or frames) and keep that meaning. The iterator parses
peer-sent packets, so a run is consumed in one step with no per-byte work.

Public API: none expected; report any change to the stats.
