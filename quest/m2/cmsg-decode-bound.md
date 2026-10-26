# [XS] Bound cmsg payload reads

## Goal

`CMsg::decode` in `rs/moq-sock/src/udp/cmsg/mod.rs` refuses a control message
shorter than its type instead of reading `size_of::<T>()` bytes past it in
release builds.

## Plan

- Today only a `debug_assert_eq!` checks the length;
  [moq-dev/moq#6678](https://github.com/moq-dev/moq/pull/6678) bounded the
  iterator but not `decode`. Return `None` when the payload is short and let
  the Unix and Windows callers skip it.

## Related

- [Wire fuzz coverage](/quest/m2/wire-fuzz-coverage.md) - fuzzes the same cmsg walk
