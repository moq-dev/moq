# [M] Halve the slowest tests' CPU

## Goal

moq's slowest 1% of tests (103 tests, 289 of 501 test CPU-seconds in one full run)
take under half that CPU, with no loss of coverage. Record per-test CPU before and
after, from one full `cargo nextest` run on the same machine.

## Plan

Decided with the maintainer on 2026-10-26, from kixelated/rust2's
[test selection report](https://github.com/kixelated/rust2/pull/69): the slowest 1%
of tests make up 57% of what either package selection or a function-level selector
would run, so cutting them beats building a selector.

- Start with moq-mux's container import tests (137 s) and moq-net's integration tests
  (42 s).
- Prefer making a test cheaper: smaller fixtures, shared setup, simulated time instead
  of real waits, fewer redundant cases.
- Moving a test to nightly only follows this epic's rule: evidence and an explicit
  coverage tradeoff for each.

Public API: none.
