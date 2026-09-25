# [M] Bindings expose epochs

## Goal

`moq-ffi`, `libmoq`, and the py, swift, kt, go, and dart wrappers publish under
the default epoch and follow bare names like Rust. The epoch of a published or
consumed broadcast is readable, and a caller can pass an explicit one. The
reconnect counter `session.epoch()` is renamed so "epoch" has one meaning.

## Plan

- Expose the parsed epoch (text and time) and an explicit-epoch publish
  argument. Keep the surface to what a binding consumer needs.
- Rename `session.epoch()` in every binding (for example to `connects()`).
  That is a break, so it lands on `dev`.
- Update `doc/lib/{py,swift,kt,go,dart,c}` per the cross-package sync table,
  and run `just test smoke --all`.

## Required

- [Origin](/quest/m1/broadcast-epoch/origin.md) - the behavior the bindings surface
