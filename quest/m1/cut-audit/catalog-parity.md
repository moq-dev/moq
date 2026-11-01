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

What the 2026-11-01 attempt
([moq-dev/moq#7518](https://github.com/moq-dev/moq/pull/7518)) found, with
prototypes on main at `a3440aa239`:

- Rust's measure is the wire's: `Subscription::max_delay` in moq-net and
  `#staleAt` in js/net judge a group by its reach, where its successor
  begins. By reach the scenario starts at the 3.0 s group, so neither
  consumer's start matches the other's or the wire's.
- The reach is a transport measure: a backlog bursts faster than real time,
  so trailing is survivable there. A player plays a buffered backlog at real
  time, so a reach start trails by up to the budget plus one group, and Rust
  never catches up, since it skips only a blocking group.
- Skipping buffered groups at any time, not only at the start, changes
  steady-state behavior that other code relies on. 12 moq-mux tests fail,
  including TS release and export tests whose own deadline accounting
  expects the consumer to never drop a frame it already buffered.
- Restricting it to the start, until the first item is read, leaves the
  steady state alone:
  - **JS's first-frame measure at the start** (prototype in
    [786c325f0c](https://github.com/moq-dev/moq/commit/786c325f0cd3bee159575198ea5557d52e83f142)):
    both start at 4.0 s. Eight moq-mux tests that wrote a backlog before
    the first read need to read first. Two moq-video dual-decoder tests
    break (`the_tail_drains_from_both_decoders`,
    `a_max_delay_decode_error_fails_the_read`): the warmup decoder's earlier
    group is fresh by its reach, but its first frame trails the newest by
    more than max + warmup, so the start drops the group the warmup needs.
    [moq-dev/moq#7311](https://github.com/moq-dev/moq/pull/7311) adds the
    container `with_warmup` and, in JS, keeps the group covering the start's
    reach through the warmup. The Rust start needs the same protection.
  - **The reach at the start in both**: both start at 3.0 s. Rust passes
    moq-mux, moq-video, and moq-audio with the same eight test edits. JS's
    `#checkMaxDelay` changes to the reach, which fails two existing hang
    tests (`Consumer raises a malformed group an age skip passes after the
    track finished`, `Consumer reads a gap when retiring an open group`).

Pending the maintainer's pick (see the PR's `## Decisions needed`), the
recommendation is JS's measure at the start, landed after
[moq-dev/moq#7311](https://github.com/moq-dev/moq/pull/7311), with the Rust
start keeping the group covering the start's reach through the warmup as
that PR's JS does. Keep the scenario as a paired test in both (the
prototype's `a_buffered_backlog_starts_within_the_budget` and
`consumer.skips.test.ts` case).
