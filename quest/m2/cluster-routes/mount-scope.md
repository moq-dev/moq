# [S] A mount files under its session's scope

## Goal

A scoped lite-07 session that closes casts no doubt on the Node ID it
declared, whether or not a mount of it is open, and a test on the route node
shows it.

## Plan

Decided 2026-11-15 (maintainer, on [the route
stream](https://github.com/moq-dev/moq/pull/8835)): ✅ a follow-up quest in
m2 (rejected: m3; ignoring it).

Pullfrog found it on the route stream. `Stream::mount` in
`rs/moq-net/src/route/node.rs` files a mount's key with `scope: None`, since
a mount hears nothing. A scoped session's close then reaches the
router as an unscoped one's, so the router doubts the Node ID the session
declared, which a scoped peer must not be trusted to speak for.

- File a mount's key with the scope of the session it belongs to, or have the
  session's close carry that scope for every key it ends.
- Test that a scoped session closing through a mount casts no doubt on its
  declared Node ID, and that an unscoped one still does.

Public API: none. Wire: none.
