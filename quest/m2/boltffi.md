# [M] Spike: BoltFFI or UniFFI as the fork's base

## Goal

A reproducible report compares BoltFFI with UniFFI on the same slice of
moq-ffi, in Python, Kotlin, and Swift, for call overhead, payload throughput,
async cost, and output size. It also estimates what each base costs to
carry. The maintainer picks which one [the hard fork](/quest/m2/uniffi-fork.md)
starts from. This quest does not adopt or fork either one.

## Plan

Decided with the maintainer on 2026-10-13:

- **Why now**: moq is hard-forking its binding generator. It is not clear
  whether UniFFI or BoltFFI is the better starting point, so this spike runs
  in parallel with other work and nothing waits on its harness.
- **Candidates**: [BoltFFI](https://github.com/boltffi/boltffi) against
  UniFFI 0.32. BoltFFI has UniFFI's proc-macro style and the same
  poll-plus-continuation async model. It ships Swift, Kotlin, Java, C#,
  Python, and wasm/TS (its own loader; since v0.31 it also runs
  wasm-bindgen for crates that use web-sys). Its README claims 1000x faster
  no-op calls than UniFFI, but those are vendor-run benchmarks. Diplomat was
  rejected: no async, no Swift or Go, no data enums, and its JS breaks on
  crates using wasm-bindgen.
- **BoltFFI's known gaps**: no Go or C++, Dart is unfinished, C is
  sync-only, and async constructors and static async methods are dropped
  (boltffi/boltffi#95). `MoqClient::connect` takes `&self` and works, but
  async constructors such as the media catalog consumer's `subscribe` hit
  it. One maintainer wrote about 80%
  of the code. Same-name types in one target probably collide (RFC #700).
  Put each gap in the report with what closing it in a fork would cost.
- **UniFFI's known costs**: three generator forks broken by every minor
  release, `Vec<u8>` copied one byte at a time, and bare-name type keys
  (mozilla/uniffi-rs#3026).
- **Slice**: connect, publish and subscribe one track, `write_frame` and
  `read_frame`, and one async read loop. Bind them with both generators side
  by side via `cfg_attr` in a spike crate, not in moq-ffi. The spike crate
  carries its own benchmarks, so it does not wait on the
  [binding benchmark](/bench/README.md). It also builds for wasm32:
  the browser comparison in #5478 reuses this slice for per-call numbers,
  so tell that quest if the crate's location or shape changes.
- **Measure**: no-op and record call overhead, payload throughput across
  frame sizes from 100 B to 1 MB, and the cost of each async await.
  Re-measure UniFFI rather than trusting BoltFFI's numbers for it.
  `just bench-ffi` gives UniFFI's numbers through the real moq-ffi. A known
  UniFFI cost to weigh: Kotlin sync calls measured 11-22 us against 1.3 us
  from Python on 2026-10-15, possibly from JNA's per-call structures
  ([Kotlin calls](/quest/m2/kt-ffi-calls.md)); async wakeups may add a JVM
  thread attach each, a separate suspect.
- **Size table**: native library, generated bindings, and wasm plus JS glue
  (gzip), for both generators. Measure the native library with moq-ffi's
  default features on and off; the toggle changes nothing on wasm32, where
  `audio` and `video` are already compiled out. Put moq-wasm in the table for reference.
- **Carry cost**: for each base, estimate the work to reach moq's eight
  targets (Python, Go, Swift, Kotlin, Dart, C++, C, browser TS), porting
  the existing Go, Dart, and C++/C generators where needed. Also estimate
  how each base's internals suit the planned fork work: bulk byte copies,
  zero-copy returns, namespaced types, and poll-native async.
- **Browser half**: BoltFFI also joins the browser generator choice in
  [#5478](https://github.com/moq-dev/moq/pull/5478) as a third candidate on
  the baseline harness. Neither blocks the other.

Public API: none. Wire: none.

## Related

- [Rust-backed @moq/net](/quest/m2/rs2ts/README.md) - the browser binding choice BoltFFI also competes in
