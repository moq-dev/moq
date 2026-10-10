# [S] A TS export stitch is bounded by the linger until its catalog arrives

## Goal

When `moq export ts --stitch` (or moq-srt egress with `stitch`) switches to
a replacement mid-stream, `--linger` bounds the wait for the replacement's
catalog, as it bounds a same-epoch return. A replacement that never delivers
its first catalog snapshot ends the export with an error instead of stalling
it with nothing going out.

## Plan

Found while iterating #5147 (2026-10-10), planned the same day. `ts::Follower`
(`rs/moq-mux/src/container/ts/follower.rs`) carries a linger deadline for a
return, which #5147 extended to the returned catalog's first snapshot. A
stitch that starts after the old broadcast ended inherits that deadline. A
mid-stream stitch, taken from `Running` while the old instance still serves,
carries none (`settling: None` in `State::Following`), so its catalog wait is
unbounded.

- Decided: the linger is the one bound, from the `Restart` to the
  replacement's first catalog snapshot. With `--linger 0` a stitch only
  succeeds when the catalog is already there. Rejected: bounding by
  `--delay`, and a fixed constant.
- On expiry the export ends with an error naming the replacement that never
  served its catalog.
- Tests, with mocked time: a stitch onto a replacement whose catalog never
  answers ends at the linger. One whose catalog answers within it switches
  as today.
- Docs: the `--stitch` and `--linger` text in `doc/bin/cli.md` and
  `doc/bin/srt.md`, including that the default `--linger 0s` lets a stitch
  follow only a replacement whose catalog is already there.

Public API: none. Behavior: a stitch can now end the export. Wire: none.
