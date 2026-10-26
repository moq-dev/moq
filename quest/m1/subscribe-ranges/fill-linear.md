# [M] The fill path is linear in range count and span

## Goal

`track_range_filling` (`rs/moq-net/benches/track.rs`) holds roughly flat
groups/s as range count and span grow.

## Plan

Measured in [moq-dev/moq#6722](https://github.com/moq-dev/moq/pull/6722): 234K
groups/s at 1 range × 1 span falls to 1.9K at 256 ranges × 64 span. Each poll's
drift edge (`live_edge`) and each candidate's successor search
(`arrival`/`successor`) walk `lookup` across all of a reader's runs, and a
filled group is never marked shown, so every scan passes it again.
`Misses::retain` also rescans every filled group per poll.

[FETCH as a range](/quest/m1/subscribe-ranges/fetch-range.md) waits on this
(maintainer, 2026-10-25, on #6722), since one-range reads would put every
past read on this path.

Public API: none. Wire: none.
