# [S] Dart: createBroadcast, setAnnounce, and announce

## Goal

The Dart wrapper, its generated `moq_ffi` bindings, tests, and
`doc/lib/dart` expose the same three announce operations as every other
binding: `createBroadcast` creates an unadvertised broadcast, `setAnnounce`
flips its exact-path advert, and `announce(prefix, route)` returns the handle
that advertises and serves requests. Nothing in Dart announces on the caller's
behalf.

## Plan

`dart/` exists only on `main`, while the FFI surface it must mirror lands on
`dev` in the bindings quest, so this cannot start until the two share a tree.
Today `dart/moq/lib/moq.dart` documents `createBroadcast` as create-and-announce
and `dart/moq/test/moq_test.dart` asserts the immediate announcement; both
change with the semantics. Regenerate `dart/moq_ffi` from `rs/moq-ffi`, adapt
the hand-written `dart/moq` wrapper, update the test to create, populate, then
`setAnnounce(true)`, add a test for `announce(prefix)` serving a request, and
update `doc/lib/dart/moq.md`.

## Required

- [#3190](/quest/m1/3190-align-origin-broadcast-creation-naming-across-language.md) - the FFI surface this wrapper mirrors
- `dev` has merged into `main`, so `dart/` and the new FFI surface are in one tree
