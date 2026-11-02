# [S] Fewer allocations on the lite request path

## Goal

`Path::concat`, and so `Path::join`, allocates once for a result over 22
bytes instead of twice, and the lite request handler keeps one copy of the
track name instead of two. Allocation-counting tests show the drop.

## Plan

Found by [moq-dev/moq#6952](https://github.com/moq-dev/moq/pull/6952):

- `Path::concat` builds a `String`, then copies it into the `Arc<str>`.
  Build it on the stack the way `from_tuple` does.
- The lite request serve copies the track name into the owned message and
  again into its log context in `RequestServe::track`.

Extend `rs/moq-net-tests/tests/announce_allocs.rs`.

Public API: none.
