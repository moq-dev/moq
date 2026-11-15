# [S] One TS republish, owned by moq-mux

## Goal

`moq import srt` and `moq import ts` republish a stepped single-program TS
source through one moq-mux implementation: the new instance keeps the
measured mux rate, stays hidden until its catalog is out, and the step reaches
the caller as a typed value rather than an error it downcasts.

## Plan

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md).
Three places mint TS instances: `container::ts::Programs` in moq-mux, the
CLI's `Republish` (`rs/moq-cli/src/publish.rs`), and moq-srt's `Instance`
(`rs/moq-srt/src/ts.rs`). [moq-dev/moq#7293](https://github.com/moq-dev/moq/pull/7293)
and [moq-dev/moq#7780](https://github.com/moq-dev/moq/pull/7780) fixed only
the CLI copy, so moq-srt's `Instance::publish` builds a default catalog on a
default, visible route, and `Import::new` re-arms the 2 s mux-rate hold on
every step. The guide (`doc/bin/ts.md`, Programs and changes) says a
republished stream "keeps the measured mux rate", which is false for SRT,
the path a live contributor uses. Both callers also find the step by
`err.downcast::<ts::Restart>()`, which AGENTS.md's "never match on errors"
forbids.

- Move the CLI's `Republish` into `container::ts` as the single-program
  sibling of `Programs`, returning the step as a variant of what decode
  returns, and have both callers use it.
- Fix `doc/bin/ts.md`'s claim that SRT carries "the same import and export":
  SRT has no passthrough.
- [Multi-program restart](/quest/m3/ts-multi-program-restart.md) then reuses
  this instead of a fourth copy.

Public API: moq-mux gains the republishing import; the `ts::Restart` error
goes if nothing else returns it. Wire: none.
