# [M] Keep moq-net's model in rust2-ts's subset

## Goal

moq-net's model, and the kio, moq-clock, and moq-pattern code it calls, stay inside the
subset [rust2-ts](https://github.com/moq-dev/rust2/blob/main/ts/README.md) translates,
so the [source-translator route](/quest/m2/rs2ts-wasm2js.md) can be measured on the model
with no hand-written stand-ins for moq's own logic.

## Plan

Decided with the rust2 maintainer on 2026-10-27, from a census of the model in
[rust2's model epic](https://github.com/moq-dev/rust2/blob/main/quest/m2/ts-model/README.md):
rust2-ts stays general, translating safe dependencies from source and refusing `unsafe`
and trait objects, so the moq-specific changes are made here. Waits until rust2's epic
reaches the model; none of these changes behavior.

- Safe twins, under `cfg(rust2_ts)`, which rust2-ts's driver passes, for the internal
  `unsafe` (not `target_arch = "wasm32"`, since moq's WASM build keeps the native ones):
  `model/group.rs`'s `MaybeUninit` frame ring, `NonNull` in `track.rs` and
  `track/demand/ranges.rs`, `cache.rs`'s pointer identity, `frame.rs`'s `Segment`, and
  kio's lock-free `log`. Differential tests cover the twins, and native builds keep the
  `unsafe` versions.
- `group.rs`'s `dyn Expiry` becomes an enum of its two implementations.
- Drop `hashlink`, `slab`, and `smallvec` from the model's path where a std type does the
  job at measured cost, or bind them to hand-written TS through rust2-ts's config.

Public API: none.

## Required

- [rust2-ts reaches moq-net's model](/quest/m3/rs2ts-model-ready.md) - the translator is ready for the model
