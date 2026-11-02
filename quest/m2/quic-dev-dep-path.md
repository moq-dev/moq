# [XS] moq-quic's internal dev-dependencies are path-only

## Goal

moq-quic's dev-dependency on moq-congestion uses a path, as every internal dev-dependency should, so publishing moq-quic never needs a matching moq-congestion release.

## Plan

Found by the agent on [moq-dev/moq#7377](https://github.com/moq-dev/moq/pull/7377): moq-quic's `[dev-dependencies]` take moq-congestion with `workspace = true`. Decided 2026-10-30 (maintainer, follow-up planning after the /quest-afk run). Switch it to a path and check no other crate has the same pattern (`cargo metadata`, or grep the `[dev-dependencies]` tables).

Public API: none. Wire: none.
