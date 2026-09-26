# [XS] JS group guard

## Goal

A `@moq/net` publisher serving a group past the subscription's max age
abandons it without an unhandled rejection, over moq-lite and IETF. Node no
longer crashes on "group exceeded the subscription max age budget".

## Plan

- `#guard` in `js/net/src/group.ts` rejects early without attaching a handler
  to the write it was handed, which the lite and IETF publishers have already
  started.
- Pass a thunk (`() => stream.write(...)`) and check expiry before calling it,
  as Rust's `poll_expired` does in `rs/moq-net/src/lite/publisher.rs`. This
  also skips writing into a group about to be abandoned. A bare
  `operation.catch(() => {})` was rejected: it still starts the write.
- `guardGroup` is internal; no public API change.
- Regression: serve an already-stale group and assert no `unhandledRejection`
  fires.

## Closes

- [#4247](https://github.com/moq-dev/moq/issues/4247) - Group.Consumer max-age guard leaves the guarded write without a handler
