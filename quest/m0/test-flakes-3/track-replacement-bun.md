# [M] Track replacement under Bun finishes every run

## Goal

`just test interop --all` passes `track replacement js-native-bun` on
WebSocket and WebTransport on every run.

## Plan

Seen while validating two PRs on 2026-11-18, and not caused by either:

- [moq-dev/moq#9053](https://github.com/moq-dev/moq/pull/9053): 1 of 4 runs
  on a clean `origin/main`, 2 of 4 on the branch, on either transport.
- [moq-dev/moq#9084](https://github.com/moq-dev/moq/pull/9084): once on
  WebSocket; the rerun passed.

The client (`test/interop/clients/js-native/replace.ts`, run by
`run_replacement_case` in `test/interop/interop.sh`) hangs until the harness
timeout instead of printing `replacement automatic=64 immediate=2 resumed=2
missing-group-fetch=2`. Every failure recorded was under Bun; whether Node
hangs too is not known.

The case runs on moq-lite-07-wip, whose relay side
[moq-dev/moq#8987](https://github.com/moq-dev/moq/pull/8987) replaced. Find
first whether the hang predates it, then whether it sits in the relay,
`js/net`, or the native addon under Bun, and fix that cause rather than the
timeout.

Public API: none. Wire: none expected.
