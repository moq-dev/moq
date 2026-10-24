# [M] TS export follows by cause

## Goal

`moq export ts --linger` and `moq export srt --linger` become `--follow
<duration>`: how long to wait for the publisher, or with `--stitch` a
replacement, to come back. The default 0 means don't follow. The Rust
`container::ts::Follower::with_linger` and moq-srt's `Subscribe::with_linger`
and `Client::with_linger` become `with_follow`. A replacement's first catalog
is awaited until it arrives or the replacement withdraws, with no deadline of
its own. A track failure is classified by its cause, not by waiting
`CLOSE_GRACE` (1s) to see whether the announcement also goes.

## Plan

Decided with the maintainer on 2026-10-23:

- One knob, one meaning. Today `--linger` also bounds the stitch catalog
  fetch (`rs/moq-mux/src/container/ts/follower.rs`, `Awaiting { deadline }`),
  so `--stitch --linger 0` fails on a cold remote replacement. The
  replacement's own lifetime is the bound instead.
- `CLOSE_GRACE` exists because the track error and the announcement removal
  arrive on different streams. Find what tells "the publisher left" from "this
  export failed" in the track's own end: the abort cause a relay gives a copy
  whose upstream went away. If no cause distinguishes them today, add one at
  the source. A wire error code change updates the drafts.
- The hidden `--linger` on the other container exports
  (`rs/moq-cli/src/args.rs`) keeps refusing; point its message at `--follow`
  only where following applies.
- Move the follower onto `moq_mux::follow::Instances` here (decided
  2026-10-24), as [readers follow restarts](/quest/m1/sink-restart-follow.md)
  did for `moq play`, `moqsrc`, and `transcode`. `with_linger` waits from the
  announcement through resolution and the first catalog, but `Instances`
  yields `Start` only after resolution, so settle what `--follow` bounds
  before moving the timer onto it. Coordinate the name with that quest.
- Docs: `doc/bin/cli.md`, plus a migration note for moq-cli, moq-mux, and
  moq-srt in the Impact bullet.

Public API: moq-mux and moq-srt renames (breaking); moq-cli flag rename.
Wire: none unless the failure cause needs a new code.
