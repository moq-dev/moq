# [S] One lifecycle vocabulary in @moq/e2ee

## Goal

`@moq/e2ee` uses the lifecycle words the way `@moq/net` does, so a publisher
wrapping its tracks and groups in encryption ends them with the same calls.

## Plan

Decided 2026-10-23 (API audit follow-up, maintainer): `finish()` declares a
clean end, `abort(err)` is terminal, `closed()` resolves on the final outcome,
and JS `close()` releases only that handle, like a Rust drop.
[moq-dev/moq#6750](https://github.com/moq-dev/moq/pull/6750) landed this in
moq-net and `@moq/net`, including consumer `close()` releasing only its handle
for groups, announcements, and broadcasts.

What remains: `@moq/e2ee` still wraps the old verbs
(`Group.Producer.close(abort?)`, `Track.Producer.close(abort?)`). Split them
into `finish()` and `abort(err)` as in `@moq/net`.

Public API: `@moq/e2ee` breaking. Wire: none.
