# BoltFFI vs UniFFI

A spike comparing [BoltFFI](https://github.com/boltffi/boltffi) 0.31.1 with
UniFFI 0.32 as the base for moq's [binding generator fork](/quest/m2/uniffi-fork.md).
The same slice of moq-ffi is bound by each generator and measured from Python and
Kotlin, with a Rust floor. Swift is generated but not run (see [Not measured](#not-measured)).

## The slice

`src/` binds moq-net directly: an origin, a broadcast, one track, groups, `write_frame`
and `read_frame` with a `MoqFrame { payload, timestamp_us }` record, an async
`recv_group` and `read_frame` loop, and a native-only QUIC client (`client` feature)
with `MoqClient::connect` and the async constructor `MoqSession::dial`. Three call
probes (`noop`, `noop_async`, `yield_once`) isolate the boundary.

The source carries BoltFFI's markers (`#[export]`, `#[data]`, `#[error]`) literally,
because BoltFFI finds them by scanning source text and does not see them inside
`cfg_attr`. The UniFFI build imports same-named attribute macros from `uniffi-shim/`,
which expand to UniFFI's. One `--features uniffi` or `--features boltffi` build picks
the generator.

The crate is its own Cargo workspace, so BoltFFI never enters the root lockfile or
`just check`. It depends on moq-net by git revision rather than path: BoltFFI scans
local path dependencies and fails on moq-tokio (see the gaps below).

## Reproduce

From the Nix dev shell (cargo, uv, npm, gradle, a JDK, wasm-bindgen):

```bash
just bench-ffi-gen              # build, bench, size
just bench-ffi-gen build bench  # or some steps: build | bench | size
```

`build` installs `boltffi_cli` 0.31.1, `tsc`, and `@boltffi/runtime` under
`target/ffi-gen`, then builds the slice with each generator, its Python, Kotlin, and
Swift bindings, both `.wasm` builds, and BoltFFI's web package. `bench` runs
`MOQ_BENCH_ROUNDS` rounds (default 3), alternating which generator goes first, and
writes `target/ffi-gen/results/report.md` and appends the table to the Actions job summary. Each case gets 3 warmup steps and a
`MOQ_BENCH_SECONDS` budget (default 1). Throughput mirrors `just bench-ffi`:
`clamp(8 MiB / size, 1, 64)` frames per group, every byte checked, and CPU from
`getrusage`. `size` builds moq-ffi and moq-wasm for reference, then prints the size
tables.

The nightly runs `build bench` and keeps `target/ffi-gen/results` as an artifact.

## Results

Measured 2026-10-16 on an Intel Core Ultra 7 270K Plus (24 threads), Linux, at moq
`b4eab0db1`. The host was shared (load average 8 to 29), so cells are medians of 3
rounds with the min-max range. Rust 1.98.1, CPython 3.14.7, OpenJDK 17 with Kotlin
2.4.20 and kotlinx-coroutines 1.11.0, JNA 5.19.1. Gain is how many times better BoltFFI
is than UniFFI.

### Calls (ns per call)

| call | Rust | Python UniFFI | Python BoltFFI | gain | Kotlin UniFFI | Kotlin BoltFFI | gain |
|---|---|---|---|---|---|---|---|
| `noop()` | 0.2 | 834 | 21.6 | 39x | 3,200 | 5.6 | 569x |
| `group.sequence()` | 0.4 | 1,920 | 63 | 30x | 7,440 | 5.8 | 1,290x |
| `track.info()`, a record return | 12 | 4,890 | 1,120 | 4.4x | 10,400 | 61 | 171x |
| `write_frame`, a 100 B record argument | 78 | 12,000 | 667 | 18x | 12,800 | 134 | 95x |
| `await noop_async()` | 0.3 | 7,190 | 5,330 | 1.3x | 8,320 | 342 | 24x |
| `await yield_once()`, one wakeup | 44 | 12,600 | 10,000 | 1.3x | 6,510 | 587 | 11x |

In Python, both generators sit at 5 to 13 us per await, which points at asyncio
rather than the boundary.

### Throughput (MB/s, write plus async read)

| frame | Rust | Python UniFFI | Python BoltFFI | gain | Kotlin UniFFI | Kotlin BoltFFI | gain |
|---|---|---|---|---|---|---|---|
| 100 B | 473 | 3.8 | 12.1 | 3.2x | 2.9 | 105 | 36x |
| 1 KiB | 3,340 | 36 | 117 | 3.3x | 36 | 954 | 27x |
| 16 KiB | 10,400 | 323 | 1,410 | 4.4x | 286 | 2,730 | 9.6x |
| 64 KiB | 8,930 | 531 | 2,840 | 5.4x | 448 | 2,760 | 6.2x |
| 1 MiB | 6,120 | 605 | 3,220 | 5.3x | 562 | 1,560 | 2.8x |

### CPU (ms per MB)

| frame | Rust | Python UniFFI | Python BoltFFI | gain | Kotlin UniFFI | Kotlin BoltFFI | gain |
|---|---|---|---|---|---|---|---|
| 100 B | 2.1 | 261 | 83 | 3.2x | 1,290 | 15 | 85x |
| 1 KiB | 0.30 | 28 | 8.6 | 3.3x | 76 | 1.2 | 61x |
| 16 KiB | 0.10 | 3.1 | 0.71 | 4.4x | 10 | 0.39 | 27x |
| 64 KiB | 0.11 | 1.9 | 0.35 | 5.4x | 3.8 | 0.37 | 10x |
| 1 MiB | 0.16 | 1.7 | 0.31 | 5.3x | 2.3 | 0.66 | 3.4x |

At 100 B, Kotlin through UniFFI spends about 130 us of CPU per frame written and read,
so 1,000 audio frames a second cost 13% of a core. BoltFFI spends 1.5 us.

### Size

Native libraries are `release` (fat LTO, `strip = "debuginfo"`, as moq-ffi ships).
"Stripped" is `strip --strip-all` on a copy. The slice includes moq-tokio's QUIC client.

| native library | as built | stripped |
|---|---|---|
| slice, UniFFI | 17,571 KiB | 14,553 KiB |
| slice, BoltFFI | 17,472 KiB | 14,482 KiB |
| BoltFFI's Python extension (`_native`), extra | 240 KiB | 64 KiB |
| BoltFFI's Kotlin JNI glue, extra | 52 KiB | 38 KiB |
| moq-ffi, default features | 34,100 KiB | 29,396 KiB |
| moq-ffi, no default features | 30,625 KiB | 26,416 KiB |

| generated bindings | files | raw | gzip |
|---|---|---|---|
| slice, Python: UniFFI / BoltFFI | 1 / 2 | 85 / 36 KiB | 12 / 7 KiB |
| slice, Kotlin: UniFFI / BoltFFI | 1 / 1 | 152 / 57 KiB | 17 / 9 KiB |
| slice, Swift: UniFFI / BoltFFI | 3 / 2 | 101 / 51 KiB | 14 / 8 KiB |
| moq-ffi, UniFFI: Python / Kotlin / Swift | 1 / 1 / 3 | 653 / 914 / 684 KiB | 66 / 73 / 72 KiB |

Browser builds use opt-level z after wasm-bindgen and no wasm-opt, as moq-wasm ships.
moq-wasm's profile strips symbols while the others keep a `name` section, so the
stripped columns drop it everywhere. moq-ffi's toggle changes nothing on wasm32, where
`audio` and `video` are already compiled out.

| browser module | .wasm as built | .wasm stripped | stripped, gzip | JS glue | JS glue, gzip |
|---|---|---|---|---|---|
| slice, UniFFI | 1,245 KiB | 515 KiB | 174 KiB | not generated | not generated |
| slice, BoltFFI | 1,148 KiB | 456 KiB | 155 KiB | 126 KiB | 25 KiB |
| moq-ffi, UniFFI | 6,218 KiB | 4,403 KiB | 1,575 KiB | not generated | not generated |
| moq-wasm, wasm-bindgen | 2,116 KiB | 2,115 KiB | 691 KiB | 38 KiB | 8 KiB |

BoltFFI's JS glue is its generated web module plus `@boltffi/runtime`, unminified.

## Not measured

- **Swift at runtime.** The dev shell has no Linux Swift toolchain. The bindings are
  generated and sized above. A Swift runner would run on the macOS job in
  `.github/workflows/platform.yml` (`macos-latest`, beside `just swift check`). The
  harness has no Swift runner yet.
- **UniFFI's JS glue.** ubrn, UniFFI's JS generator, does not support 0.32 yet, so only
  its `.wasm` is sized. The browser comparison, including its per-call cost, is
  [`test/wasm/generator.md`](/test/wasm/generator.md).
- **The full moq-ffi through BoltFFI.** BoltFFI cannot bind moq-ffi without the API
  changes listed below, so its numbers come from the slice alone.
- Go, Dart, C++, and C, which BoltFFI lacks or has only in part. Startup and memory.

## Where the gap comes from

The async model is the same: both generators poll a Rust future and resume it from a
continuation. The difference is in each backend's calling convention.

- **Python**: UniFFI calls through `ctypes` and serializes records into a
  `RustBuffer`. BoltFFI generates a CPython C extension, so a call is a C function
  call.
- **Kotlin**: UniFFI calls through JNA, which builds per-call structures by reflection
  ([Kotlin calls](https://github.com/kixelated/rust2/blob/main/quest/m1/kt-ffi-calls.md)). BoltFFI generates JNI glue and passes
  buffers as direct `ByteBuffer`s. BoltFFI's 6 ns no-op call is about the JNI floor.
- **Bytes**: UniFFI copies `Vec<u8>` one byte at a time
  ([Payload bytes](https://github.com/kixelated/rust2/blob/main/quest/m1/ffi-bytes.md)). BoltFFI copies in bulk, which is why its
  advantage grows with frame size in Python.

None of these needs BoltFFI's core. Each is a backend change that a UniFFI fork can
make, and BoltFFI's numbers are the targets to hit.

## Gaps found in BoltFFI 0.31.1

Each was hit while binding the slice, about 300 lines.

| gap | workaround in the slice | cost to close in a fork |
|---|---|---|
| The scanner does not see markers inside `cfg_attr` | literal markers plus `uniffi-shim/` | S: expand `cfg_attr` in the scanner |
| The scanner ignores target `cfg`, so `cfg(target_arch = "wasm32")` items are bound everywhere | a `client` cargo feature | M: evaluate target cfgs while scanning, which moq-ffi's wasm gating needs |
| It scans any local path dependency with an item-level `#[error]`, so thiserror crates count as BoltFFI crates | git dependencies | S |
| It resolves `#[path]` from the wrong directory and fails on moq-tokio | git dependencies | S |
| `#[error]` collides with thiserror's attribute | a hand-written `Display` | S, or rename the marker |
| `Arc<T>` is rejected in signatures | classes by value, arguments by `&T` | M: moq-ffi returns `Arc` everywhere |
| The Python backend cannot return `Option<Class>`, and by default it skips the whole class silently | `recv_group` returns `Ended` as an error; `--deny-skipped` | S to M |
| Classes must be `Send + Sync` on wasm, and `single_threaded` cannot be target-conditional | `unsafe impl Send/Sync` on wasm32 | M |
| A panic inside an async export aborts the process ("panic in a function that cannot unwind"); UniFFI raises `InternalError` | fixed the panic | S: `catch_unwind` in the generated poll |
| Its macros emit the FFI only under its CLI's build (`--cfg boltffi_binding_expansion`), so a plain `cargo build` exports nothing, and the CLI accepts only `debug` and `release` profiles | `CARGO_PROFILE_RELEASE_OPT_LEVEL=z` | S |
| `pack wasm` needs `wasm-opt`, `tsc`, and `@types/node` on hand | installed under `target/ffi-gen` | S |

Async constructors work: `MoqSession::dial` binds in Python, Kotlin, Swift, and TS, and
its error path ran from Python under both generators, although
[boltffi#95](https://github.com/boltffi/boltffi/issues/95) is still open. TS types it as
`Promise<MoqSession | null>`.

## UniFFI costs found

- An error field named `message` clashes with `Throwable.message` in Kotlin, so the
  slice uses `reason`.
- Two `#[cfg]`-gated variants of one method inside an exported `impl` both emit
  UniFFI's scaffolding, so its symbols are defined twice.
- `Option<Object>` needs `Option<Arc<T>>`.
- The known ones: three external generator forks that break on each minor release, the
  per-byte `Vec<u8>` copy, bare-name type keys
  ([mozilla/uniffi-rs#3026](https://github.com/mozilla/uniffi-rs/issues/3026)), and the
  call overhead above.

## Carrying cost

### moq's eight targets

| target | from UniFFI | from BoltFFI |
|---|---|---|
| Python | in core | in core |
| Kotlin | in core | in core |
| Swift | in core | in core |
| Go | `kixelated/uniffi-bindgen-go`: import | none: write a backend, L |
| Dart | `kixelated/uniffi-dart`: import | unfinished: finish it, M to L |
| C++ | `kixelated/uniffi-bindgen-cpp`: import | none: write a backend, L |
| C | `uniffi-bindgen-cpp`'s C backend: import | sync only: add async, M |
| browser TS | ubrn, once it supports 0.32 | in core, with its own loader and runtime |

BoltFFI's existing backends run 6k (C) to 18k (Python) lines each, so each missing
backend is a similar rewrite rather than a port: UniFFI's generators are template-based
and do not translate to BoltFFI's IR.

### Planned fork work

| work | from UniFFI | from BoltFFI |
|---|---|---|
| Bulk byte copies | M: [Payload bytes](https://github.com/kixelated/rust2/blob/main/quest/m1/ffi-bytes.md) | done |
| Call overhead | M: JNI or JNA direct mapping for Kotlin ([Kotlin calls](https://github.com/kixelated/rust2/blob/main/quest/m1/kt-ffi-calls.md)); L: a C-extension Python backend, not yet planned | done |
| Zero-copy byte returns | M: [Zero-copy byte returns](https://github.com/kixelated/rust2/blob/main/quest/m2/ffi-bytes-return.md) | M: bytes cross its wire buffer, with no borrowed return |
| Namespaced types | M: [Namespaced external types](https://github.com/kixelated/rust2/blob/main/quest/m2/uniffi-namespaces.md) | M: same-name types collide (RFC #700, open) |
| Poll-native async | M: [Poll-native async](/quest/m2/ffi-poll-migrate.md) | M: the same poll-plus-continuation model, with per-method poll exports |
| Binding moq-ffi as written | none | M: drop `Arc` returns, rework wasm gating, and fix the gaps above |

Either way the fork owns a large codebase. BoltFFI's is about 134k lines, written mostly
by one maintainer, and is at 0.31 with breaking minor releases.

## Recommendation

Fork **UniFFI**. BoltFFI is much faster, with 30 to 1,300 times cheaper sync calls and
3 to 36 times higher throughput, and slightly smaller. But it lacks Go and C++, has only
part of Dart and C, and broke in eleven ways on a 300-line slice before moq-ffi's real
surface. Its speed comes from backend calling conventions (a CPython extension, JNI, and
bulk copies), not from its core. A UniFFI fork can adopt each one backend at a time,
using these numbers as targets, while keeping all eight targets working throughout.

Pick BoltFFI instead if call overhead outweighs everything else and writing the Go and
C++ backends is acceptable. The browser half was decided separately: UniFFI over ubrn
([#5838](https://github.com/moq-dev/moq/pull/5838), [`test/wasm/generator.md`](/test/wasm/generator.md)).
