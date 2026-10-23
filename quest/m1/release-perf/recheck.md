# [M] Re-check

## Goal

Every screen-only move in the [epic](/quest/m1/release-perf/README.md) is
re-measured with instruction counts. Each one that holds is fixed in this
quest's PR, one commit per fix, or accepted with a reason recorded in the
epic.

## Plan

Use the instruction-count method from the [performance audit](/quest/m2/audit-perf.md).
Wall time on a shared host can't tell these apart. Build each bench at
`origin/release` and at main, and compare instructions per iteration. A
change that needs a maintainer decision, or is too large to review alongside
the rest, becomes a follow-up through `/quest-plan`.

Leads from reading code on 2026-10-21, strongest first:

1. moq-mux `container-export/{ts,mkv}/4x1` (4.7x, 3.9x). The worst ratio is at
   one frame per track, so look for a fixed cost per export or per track. The
   reap cost from [#5942](https://github.com/moq-dev/moq/pull/5942) that the
   audit found grows with buffered groups instead. Suspects: every export now
   goes through `jitter::Buffer`
   ([#4645](https://github.com/moq-dev/moq/pull/4645),
   [#5309](https://github.com/moq-dev/moq/pull/5309)), and `container/source.rs`
   start-up ([#5947](https://github.com/moq-dev/moq/pull/5947),
   [#5854](https://github.com/moq-dev/moq/pull/5854)). If the jitter path
   dominates, skip it when the delay is zero.
2. `annexb-import` (median 2.1x). Check `group_*` and `append_group` first. If
   they regressed by the same factor, the cost is per group in moq-net
   ([#5803](https://github.com/moq-dev/moq/pull/5803) rewind check,
   [#5725](https://github.com/moq-dev/moq/pull/5725) largest notifications,
   [#6020](https://github.com/moq-dev/moq/pull/6020)), not in the importer.
3. `track_stamp_groups` (median 2.7x). The first frame on a timed track now
   takes the track lock and `bounds`, `starts`, and `sequence` mutexes
   ([#5803](https://github.com/moq-dev/moq/pull/5803),
   [#6074](https://github.com/moq-dev/moq/pull/6074)). Pass the checked start
   into `first_frame`, merge `bounds` and `starts` under one lock, and skip
   `advance_largest`'s write and wake when nothing changed.
4. `track_datagram_subscriber_join` (median 2.3x). `Cursor::new` scans the
   datagram ring twice ([#5195](https://github.com/moq-dev/moq/pull/5195)).
   Track the newest datagram timestamp on push. The window's construction cost
   from [#5929](https://github.com/moq-dev/moq/pull/5929) was cut in
   [#6112](https://github.com/moq-dev/moq/pull/6112); an inline single-range
   window ([#6241](https://github.com/moq-dev/moq/pull/6241)) regressed join
   time, so the rest stays.
5. `track_parallel_write`, `track_fanout_frame`, `track_empty_run`,
   `group_write_frames` (1.2 to 1.6x). This is constant per-group and
   per-frame cost from several features. Resolve `now` once per poll in
   `Drift`, and skip no-op `advance_largest` writes. Accept the rest.
6. `origin/announce_duplicate/30d_1s` (4.3x). Compare against `1d_1s`, to
   tell a constant cost per change from a cost per duplicate. Leads: an
   epochless winner switch is now a `Restart`
   ([#5087](https://github.com/moq-dev/moq/pull/5087),
   [#5443](https://github.com/moq-dev/moq/pull/5443),
   [#4942](https://github.com/moq-dev/moq/pull/4942),
   [#5144](https://github.com/moq-dev/moq/pull/5144)).
7. `session_delivery_{relays,paced,scale}` (1.4 to 1.6x). Expected to be the
   per-group cost above plus the flow-controlled mock transport
   ([#5530](https://github.com/moq-dev/moq/pull/5530)). Re-measure after the
   fixes above, and accept what remains.
8. moq-stats `aggregate` at live=16, departed=4096, and kio
   `contended_{notify,log}` at 16 threads: confirm they are noise. If the
   aggregate row holds, suspect [#4874](https://github.com/moq-dev/moq/pull/4874)
   and [#4904](https://github.com/moq-dev/moq/pull/4904).

Add a bench sweep wherever a fix removes a slope, so the slope stays visible.
