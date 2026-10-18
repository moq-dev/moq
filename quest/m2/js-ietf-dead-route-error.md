# [XS] js/net names the real cause of a dead route on IETF

## Goal

When a moq-transport session dies under a JS subscription, the subscription
ends with the session's own error, as it does in Rust, instead of
`unauthorized: <broadcast>`, such as `unauthorized: room`. A grant
that lapses because its session closed is not a revocation.

## Plan

Decided with the maintainer on 2026-10-17: the goal as drafted, from the
follow-ups of [#5700](https://github.com/moq-dev/moq/pull/5700).

The cause, as read at planning: `#close` in `js/net/src/ietf/connection.ts`
closes the session's auth first, which empties the grant union
(`#recompute` in `js/net/src/auth_session.ts`). The IETF subscriber's grant
watcher (`#denied` in `js/net/src/ietf/subscriber.ts`) then closes each live
subscription with `unauthorized(broadcast)` before anything ends it with the
session error. Rust has the same grant shape, but its IETF driver
(`rs/moq-net/src/ietf/session.rs`) aborts every received track with the
session's error in the same pass that closes auth, so the session error wins.

Check the lite subscriber for the same race. Add a test that kills an IETF
session under a live subscription and asserts the session error, failing
without the fix. Wire: none.
