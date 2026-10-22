# [S] Interop covers a clean broadcast finish

## Goal

The interop matrix has a media "finish" case: a publisher in each
implementation finishes its broadcast cleanly, and every subscriber drains
the tail and then sees the broadcast end, rather than an abort. A regression
in any implementation's linger shows up as a red cell.

## Plan

Rust, JS ([moq-dev/moq#6151](https://github.com/moq-dev/moq/pull/6151)),
and the bindings ([moq-dev/moq#6157](https://github.com/moq-dev/moq/pull/6157))
each have a finish. Drive it from the existing
interop clients rather than adding a new harness, and assert both halves:
the late tail arrives and the end is clean. The new case's cells show up
in the nightly report at <https://git.moq.dev/interop/>.

Public API: none. Wire: none.
