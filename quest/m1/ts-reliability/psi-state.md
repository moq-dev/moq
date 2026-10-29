# [M] The TS export lays its tables from current state

## Goal

The TS export lays its PAT and PMT from the current table state when their
slot comes up, as `--sync` already does, instead of carrying queued copies
alongside media units. A static program's output is byte-identical to today.

## Plan

Planned 2026-10-28 (maintainer). The maintainer named this rework when
closing [moq-dev/moq#6301](https://github.com/moq-dev/moq/pull/6301): the
export's table handling from
[moq-dev/moq#6289](https://github.com/moq-dev/moq/pull/6289) queues copies of
the PAT and PMT with media units, so a table change rewrites queued copies.
Laying each table from current state when its slot is laid makes a change
one more state update, which [PAT changes](/quest/m1/ts-reliability/pat-lifecycle.md)
builds on.

- Reuse what `--sync` already does to lay tables, rather than adding a second
  path.
- Keep #6289's revision rule: a PMT revision goes out at the next version.
- Prove byte-identical output on the existing export fixtures and round
  trips, and keep the T-STD schedule and the opening order from
  [moq-dev/moq#7048](https://github.com/moq-dev/moq/pull/7048) unchanged.

Rust: `rs/moq-mux` TS export (`build_psi`, the PSI schedule).

Public API: none. Wire: none.
