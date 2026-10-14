# [M] Spike: BoltFFI beside UniFFI

## Goal

A reproducible report compares BoltFFI with UniFFI on the same slice of
moq-ffi, in Python, Kotlin, and Swift, for call overhead, payload throughput,
async cost, and output size. The maintainer then decides whether to keep
UniFFI, replace it, or run both. This quest does not adopt BoltFFI.

## Plan

Decided with the maintainer on 2026-10-13:

- **Candidates**: [BoltFFI](https://github.com/boltffi/boltffi) only. It has
  UniFFI's proc-macro style and the same poll-plus-continuation async model.
  It ships Swift, Kotlin, Java, C#, Python, and wasm/TS (its own loader;
  since v0.31 it also runs wasm-bindgen for crates that use web-sys). Its
  README claims 1000x faster no-op calls than UniFFI, but those are
  vendor-run benchmarks. Diplomat was rejected: no async, no Swift or Go, no
  data enums, and its JS breaks on crates using wasm-bindgen.
- **Known gaps to report, not fix**: no Go or C++, Dart is unfinished, C is
  sync-only, and async constructors and static async methods are dropped
  (boltffi/boltffi#95, which `connect` hits). One maintainer wrote about 80%
  of the code. Same-name types in one target probably collide (RFC #700).
- **Slice**: connect, publish and subscribe one track, `write_frame` and
  `read_frame`, and one async read loop. Bind them with both generators side
  by side via `cfg_attr` in a spike crate, not in moq-ffi.
- **Measure** on the [payload harness](/quest/m1/ffi-bytes.md): no-op and
  record call overhead, payload throughput across frame sizes, and the cost
  of each async await. Re-measure UniFFI rather than trusting BoltFFI's
  numbers for it.
- **Size table**: native library, generated bindings, and wasm plus JS glue
  (gzip), each with moq-ffi's default features on and off, for both
  generators. Put moq-wasm in the table for reference.
- **Browser half**: BoltFFI joins the browser generator choice in
  [#5478](https://github.com/moq-dev/moq/pull/5478) as a third candidate on
  the baseline harness. This quest owns the native half and the combined
  size table. Neither blocks the other.

Public API: none. Wire: none.

## Required

- [Payload bytes](/quest/m1/ffi-bytes.md) - the per-wrapper benchmark harness

## Related

- [Rust-backed @moq/net](/quest/m2/rs2ts/README.md) - the browser binding choice BoltFFI also competes in
- [Hard-fork UniFFI](/quest/m1/uniffi-fork.md) - the alternative to switching generators
