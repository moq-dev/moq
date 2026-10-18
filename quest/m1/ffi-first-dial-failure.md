# [S] moq-ffi reports why a first dial fails

## Goal

While a moq-ffi client's first dial keeps failing and retrying, the caller can
read why, so the OBS dock names the reason instead of showing only
"Connecting…". moq-c already does this: `moq_session_connect` returns a
session before the first connect, and `moq_session_failure` names the
retrying dial.

Today the native `connect` in `rs/moq-ffi/src/session.rs` awaits
`connection.established()` before it returns a `MoqSession`, so
`MoqSession::failure()` (#5723) is unreachable until the first success.

## Plan

Decided with the maintainer on 2026-10-17: the goal as drafted, in m1
because OBS needs it.

The API shape is the maintainer's call; bring options with a recommendation.
Candidates: a failure accessor on `MoqClient` readable while `connect()` is
pending, returning the session before the first connect as moq-c does, or
failing the first dial fast with its error. The first is additive and keeps
`connect()` surfacing auth refusals. Follow the moq-ffi row of the
Cross-Package Sync table, ending with the dock in `cpp/obs`.

Test: a dial to a listener that hangs up reports its reason before any
session connects, and the report clears once one does.
