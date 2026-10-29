# [M] JS request and follow

## Goal

`@moq/net` exposes `request(path, {epoch})` and `follow(path, {epoch})`
instead of the `announced` option, and its in-package callers use them.

## Plan

Split from [Two broadcast resolutions](/quest/m1/api-audit/resolve/README.md)
on 2026-10-28 (maintainer); its Plan holds the behavior already landed in
[moq-dev/moq#6445](https://github.com/moq-dev/moq/pull/6445).

- Keep the landed synced-absence `Unroutable` for `request` while `follow`
  stays open, and its pending retry through a route gap without retargeting a
  resolved instance.
- Retain synchronous request peeks and path-local wakeups.
- `Producer` pass-throughs stay only where `Table` needs them.
- Mirror the Rust names from [Rust request and follow](/quest/m1/api-audit/resolve/rust.md).

Tests: synced absence, no follower fallback on a blind peer, pinned follow
return, and discovery retention and release, on mocked time.

Public API: breaking in `@moq/net` (the `announced` option goes), with an
Impact migration note and a regenerated API report. Wire: none.
