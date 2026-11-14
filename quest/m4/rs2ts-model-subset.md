# [M] Keep moq-net's model in rust2-ts's subset

## Goal

moq-net's model, and the kio (clock included) and moq-pattern code it calls, stay inside the
subset [rust2-ts](https://github.com/moq-dev/rust2/blob/main/ts/README.md) translates,
so the [source-translator route](/quest/m5/rs2ts-js.md) can be measured on the model
with no hand-written stand-ins for moq's own logic.

## Plan

Decided with the rust2 maintainer on 2026-10-27, from a census of the model in
[rust2's model epic](https://github.com/moq-dev/rust2/blob/main/quest/m2/ts-model/README.md):
rust2-ts stays general, translating safe dependencies from source and refusing `unsafe`
and trait objects, so the moq-specific changes are made here. Waits until rust2's epic
reaches the model; none of these changes behavior.

- `Track` and `FrameContent` take a unique id at construction, a `u64` from a counter, and
  key `cache.rs`'s deadlines and `frame.rs`'s per-group frames by it in place of their
  addresses (`Arc::as_ptr(..) as usize`, and `ptr::from_ref(self) as usize` in `Drop`).
  rust2-ts refuses every address read, while `ptr::eq` and `Arc::ptr_eq` translate.
- The internal `unsafe`: `model/group.rs`'s `MaybeUninit` frame ring, `NonNull` in
  `track.rs` and `track/demand/ranges.rs`, `frame.rs`'s `Segment`, and kio's lock-free
  `log`. Each is either rewritten in safe Rust for both builds, at measured native cost, or
  left for rust2-ts to translate as is where its
  [unsafe policy](https://github.com/moq-dev/rust2/blob/main/quest/m3/README.md) allows,
  which refuses any `unsafe` needing a new pointer form.
- `group.rs`'s `dyn Expiry` becomes an enum of its two implementations.
- Drop `hashlink` and `slab` from the model's path where a std type does the
  job at measured cost, or bind them to hand-written TS through rust2-ts's config.
  `smallvec` stays: rust2-ts maps it into its runtime instead
  ([rust2's smallvec quest](https://github.com/moq-dev/rust2/blob/main/quest/m2/ts-model/smallvec.md),
  decided with the rust2 maintainer on 2026-11-07).

Decided with the rust2 maintainer on 2026-11-07, in
[rust2's model epic](https://github.com/moq-dev/rust2/blob/main/quest/m2/ts-model/README.md),
after rust2-ts's designs for addresses as identity
([moq-dev/rust2#596](https://github.com/moq-dev/rust2/pull/596)) kept leaking:

- ✅ Explicit ids for the two address keys, the same in both builds (rejected: a
  `cfg(rust2_ts)` twin; rust2-ts translating `Arc` addresses).
- ✅ No `cfg(rust2_ts)` twins anywhere: every change here is the same in native and
  translated builds (rejected: twins until rust2-ts translates the `unsafe`).
- ✅ Whether each `unsafe` item is rewritten or waits on rust2-ts is decided per item when
  this quest starts (rejected: deciding now).

Public API: none.
