# [M] TS SI tables repeat on the schedule's grid

## Goal

While media flows, `export ts` keeps every SI table (SDT, NIT, EIT, BAT, TDT, TOT) inside its TR 101 290 repetition limit on the wire, whatever the send-ahead of the frames carrying it, as PAT and PMT already do.

## Plan

[moq-dev/moq#9185](https://github.com/moq-dev/moq/pull/9185) put PAT and PMT on the schedule's absolute repeat grid but left SI frame-led: `SiTrack::emit` in `rs/moq-mux/src/container/ts/export.rs` anchors `last_emit` on the carrying frame's media timestamp and repeats at half the declared interval. A copy leaves up to its frame's send-ahead early, so two consecutive copies land more than the limit apart when their frames' send-ahead differs by more than half of it, such as video with over a second of send-ahead followed by audio alone.

Give the schedule (`rs/moq-mux/src/container/ts/schedule.rs`) each SI PID's sections and cadence, so it repeats them on wire time like PAT and PMT, while revisions still go out in the unit that puts them in force. Extend `table_repeats_keep_inside_their_limits_on_the_wire` with a send-ahead spread that fails today.
