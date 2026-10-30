# [S] Rust and JS skip a backlog by the same measure

## Goal

A player joining a backlog starts at the same group in moq-cli, moqsrc, and
the browser. If they start at different groups, align the two container
consumers' skip measurements, with a test in each.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. The catalog half of
this quest (Rust and JS accept the same catalogs) landed separately in
[moq-dev/moq#7035](https://github.com/moq-dev/moq/pull/7035): any malformed
rendition refuses the whole catalog in both languages (maintainer,
2026-10-30).

Rust measures newest content minus the next group's start
(`rs/moq-mux/src/container/consumer.rs`, the `should_skip` check in the
blocking-group path). JS measures from the oldest unread frame
(`js/hang/src/container/consumer.ts`, `#checkMaxDelay`). The Rust comment
argues for its measure: a group's own first frame is no bound, since a group
longer than the budget would be cut short whenever it blocks behind a newer
one, as a join's backlog does.

They diverge, reproduced by a throwaway probe in each consumer: five
finished one-second groups of ten frames plus a one-frame sixth group, all
landing before the first read, with a 1500 ms budget. JS starts at the 4.0 s
group, while Rust starts at 0 and plays the whole backlog, whether the sixth
group is open or finished. Rust's skip only runs while the current group
blocks, so a fully buffered backlog never triggers it.

Decide which start is right (likely JS's, which keeps the join within the
budget), align the other consumer, and keep the scenario as a paired test in
both.
