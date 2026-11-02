# [S] @moq/net's close-drain tests survive load

## Goal

The `js/net/src/lite/connection.test.ts` close-drain cases, including "a peer
close drains the overtaken fin over moq-lite-0x", pass on every full
`@moq/net` suite run. They're fixed at their cause, never by raising a
timeout or adding a retry.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). The cases fail 3 to 4 times per full-suite run on main and pass when the
file runs alone. Three agents hit this on 2026-11-01:
[moq-dev/moq#7367](https://github.com/moq-dev/moq/pull/7367),
[moq-dev/moq#7489](https://github.com/moq-dev/moq/pull/7489) (3 of 570 runs on
main), and [moq-dev/moq#7296](https://github.com/moq-dev/moq/pull/7296)'s
area. One failure fails a whole land batch's `test js`.

Look for wall-clock waits, or ordering that depends on the event loop's load,
and drive them with the manual clock.

Follow the rules in [More tests under load](/quest/m0/test-flakes-2/README.md).
