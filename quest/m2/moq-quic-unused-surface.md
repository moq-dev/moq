# [S] moq-quic drops inherited surfaces with no consumer

## Goal

moq-quic's public API loses the inherited surfaces nothing calls:
`endpoint::{Accepting, Accepted}`, `Endpoint::{start_accept, finish_accept}`,
`Connection::{side, is_handshaking}`, and `stream::Id::{new, dir, initiator}`.
Each is removed, not just made private, unless a caller inside moq-quic needs
it, in which case it becomes private. `MIGRATE.md` lists the removals.

## Plan

Decided 2026-11-07 (maintainer, /quest-plan from
[moq-dev/moq#8088](https://github.com/moq-dev/moq/pull/8088)): remove them
before the cut, since removing public API after it would be another break.
Not chosen: folding the list into the post-cut
[dead-code audit](/quest/m3/audit-dead-code.md).

Confirm each has no caller across the workspace, bindings, and vendored
crates before removing it.
