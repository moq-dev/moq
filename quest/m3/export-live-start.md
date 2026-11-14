# [S] Container export starts apart from its skip budget

## Goal

An fMP4 or MP4 export starts at the live edge even when it is allowed to fall
behind by a long skip budget, and reports how many samples it wrote per
track.

## Plan

Decided 2026-11-05 (maintainer, on moq-dev/studio's /quest-plan): m3, since the studio is a named consumer that is not urgent.

`with_delay` (`rs/moq-mux/src/container/fmp4/export.rs`, and
`container/mp4.rs`) takes a `Delay { min, max }`, but the start still follows
the max: the container consumer starts at the lowest group that arrived and
skips only groups trailing the newest content by more than `max`
(`rs/moq-mux/src/container/consumer.rs`), so it reads from the oldest group
within that window whatever `min` says. The studio
recorder ([moq-dev/studio#18](https://github.com/moq-dev/studio/pull/18)) sets 2 s so a disk stall loses nothing,
and so every recording also starts up to 2 s before it was started
(`rs/moq-studio/src/record.rs`). Only TS export reports stats today; fMP4 and
MP4 report no sample counts, which the recorder needs to say what it wrote.

Changed 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): [moq-dev/moq#8298](https://github.com/moq-dev/moq/pull/8298) (merged) gives fMP4, MP4, MKV, and FLV a `moq_net::track::Delay { min, max }` before the cut, so splitting the start from the skip budget is an implementation fix, not a break.
