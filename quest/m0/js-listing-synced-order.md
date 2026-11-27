# [S] Fix JS Tracks listing SYNCED ordering and scope moq-transport SYNCED guarantee

## Goal

1. In `@moq/net` (`js/net/src/listing.ts`), re-adding a queued track before the
   reader drains never shifts another pre-`SYNCED` queued track after `SYNCED`
   (matching Rust's queue accounting).
2. In `drafts/draft-lcurley-moq-lite.md` and the Rust `Tracks` comments, scope
   the upstream complete-list `SYNCED` guarantee to `moq-lite` upstreams that
   signal completion, noting that a relay with a `moq-transport` upstream
   reports only what it knows at that hop.

## Plan

Found 2026-11-27 by replaying `gpt-6.2-sol` (`medium`) on
[moq-dev/moq#9796](https://github.com/moq-dev/moq/pull/9796), which merged
unreviewed under the `low` tier.

- Caused by: [moq-dev/moq#9796](https://github.com/moq-dev/moq/pull/9796)
  (list under `## Caused by` in the fixing PR).
- In `js/net/src/listing.ts`, when a reader has already seen `a`, `-a` and `+b`
  are queued when upstream signals `SYNCED`, and `a` is re-added before the
  queue drains, the removal splits into two events without increasing the count
  of events ahead of `SYNCED`. The reader then observes `-a`, `+a`, `SYNCED`,
  `+b` and misses `b` even though it was listed at `SYNCED`. Match Rust's queue
  accounting in `rs/moq-net/src/model/listing.rs` and port its regression test.
- In `drafts/draft-lcurley-moq-lite.md:247` and `rs/moq-net/src/lite/tracks.rs`
  / `rs/moq-net/src/model/listing.rs` comments, clarify that `SYNCED` waits on
  upstream completion only for `moq-lite` upstreams; a relay bridged to a
  `moq-transport` upstream signals `SYNCED` with only the tracks it already
  knows about, and later `PUBLISH` offers arrive as live additions.

Tests: JS unit test in `js/net` covering `-a`, `+b`, `SYNCED`, `+a` before
drain and asserting `+b` arrives before `SYNCED`, plus `just check`.
