# [M] A JS relay asks a pre-07 upstream for every reader's join, as Rust's front does

## Goal

A JS origin relaying a track from a lite-06 or older upstream asks that upstream
for the fold of every reader's ranges, as Rust's front does
(`rs/moq-net/src/model/resume.rs` `Producer::follow`). A later reader whose live join
reaches past every group then gets every group the upstream still holds, not
just those at or above where an earlier reader's join landed.

## Plan

Found on [moq-dev/moq#9409](https://github.com/moq-dev/moq/pull/9409), which made a
range with no start a live join and made a pre-07 SUBSCRIBE_UPDATE restate the
subscription. `js/net/src/lite/drop_record.test.ts` has a test, "through a lite-06
upstream, a group reported dropped as it lands is fetched for a widened lite-07
viewer". In it, the second reader gets 5..9 where Rust's counterpart,
`drop_record::a_lite06_upstream_drop_is_fetched_for_a_widened_lite07_viewer`, gets
0..9. That PR skips the JS test, pointing here.

Rust's relay serves every origin through a front. The front asks each copy for its
readers' live joins, placed where the copy put them, so the copy's lite-06 start
moves twice: to the first reader's placement, then to none for the second reader.
JS serves an ingest broadcast directly, and builds a front only for announcements
that carry an instance. On that plain path the copy's requested start never moves,
so the old SUBSCRIBE_START of 5 stands for the second reader.

Port the fold so a JS copy restates its pre-07 upstream subscription when a reader's
join reaches below the start it was answered with. That means either JS origins
serving through a front, or the same fold on the plain path. Moving every JS origin
onto a front is the larger change; it broke "a run of billions of sequences is
settled by each reader in one step" when tried. Then un-skip the test.

Public API: none. Wire: none.
