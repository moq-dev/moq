# [S] Interop covers a clean broadcast finish

## Goal

The interop matrix has a media "finish" case: a publisher in each
implementation finishes its broadcast cleanly, and every subscriber drains
the tail and then sees the broadcast end, rather than an abort. A regression
in any implementation's linger shows up as a red cell.

## Plan

From [moq-dev/moq#6151](https://github.com/moq-dev/moq/pull/6151), which
gave js/net `Broadcast.Producer.finish()`. Rust, JS, and the bindings each
have a finish once their linger quests land. Drive it from the existing
interop clients rather than adding a new harness, and assert both halves:
the late tail arrives and the end is clean.

Public API: none. Wire: none.

## Required

- [Binding linger](/quest/m1/linger-ffi.md) - bindings finish a broadcast, so the binding interop clients can
- [JS linger](/quest/m1/linger-js.md) - js/net finishes a broadcast, so the JS interop client can

## Related

- [Interop report](/quest/m1/interop-report.md) - where the new case's cells show up
