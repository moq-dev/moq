# Browser binding generator report

Which Rust binding `@moq/net` wraps in the browser: moq-ffi through UniFFI (ubrn's
`wasm2` player), moq-ffi through BoltFFI, or the hand-written wasm-bindgen `rs/moq-wasm`.
This compares them on the [baseline harness](README.md#backend-baseline).

BoltFFI cannot bind moq-ffi as written (see [What blocks BoltFFI](#what-blocks-boltffi)),
so its network numbers come from the [BoltFFI spike](https://github.com/moq-dev/rust2/blob/main/bench/ffi/README.md#boltffi-comparison)'s slice with a browser client added. The
same slice also runs through UniFFI, so the two generators compare on identical Rust.

**Decision** (maintainer, 2026-10-17, [#5838](https://github.com/moq-dev/moq/pull/5838)):
moq-ffi through UniFFI over ubrn. It keeps one binding surface for every language with no
API break, and passes every parity case. Its two measured costs have follow-ups:
[size](/quest/m2/rs2ts/net-only.md) and [payload copies](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-bytes.md).
[Complete WASM bindings](/quest/m2/rs2ts/wasm-bindings.md) builds on this harness.

## Reproduce

```bash
just test wasm-baseline --uniffi   # JS, moq-wasm, and moq-ffi in the baseline sweep, plus parity cases and per-call timings
just test wasm --uniffi            # the parity cases and per-call timings alone
bun test/wasm/uniffi/size.ts <module.wasm>...   # sections and per-crate code share
```

ubrn 0.31.0-6 reads UniFFI 0.31 metadata and moq-ffi is on 0.32, so
[`uniffi/build.sh`](uniffi/build.sh) applies [`uniffi/pin.patch`](uniffi/pin.patch) to a
scratch checkout. The pin is measurement-only and does not land; moq-ffi built on 0.31 with
no source change beyond linking ubrn's runtime.

The slice columns came from a harness removed once UniFFI was chosen. It is preserved at
[`9f557ac2b`](https://github.com/moq-dev/moq/tree/9f557ac2ba274508b3ad3b9f6910cc0861632b6f/test/wasm/slice)
(`just test wasm-baseline --uniffi --slice` there). It copied `bench/ffi-gen` from
[#5736](https://github.com/moq-dev/moq/pull/5736) and applied `slice/browser.patch`, a
`browser` feature with a WebTransport session that consumes and publishes like moq-ffi's
wasm client. Then it bound the result with both generators.

## Setup

Measured 2026-10-16 at `3789ee2ef` on an AMD Ryzen 9 5900XT (32 threads), Linux, headless
Chromium 153.0.8010.12. The host was shared with other agents (load average 12 to 32), so
treat absolute numbers as noisy and compare within a table. Every comparison interleaves
its candidates. The sweep runs each shape three times per backend in the order js, wasm,
uniffi, slice-uniffi, slice-boltffi, then reversed, then forward again. The per-call bench
runs ABBA rounds on fresh pages. Cells are medians with the min to max range.

All builds use `wasm-release` (opt-level z, fat LTO) and no wasm-opt, as moq-wasm ships.

## Size

Shipped assets, as each backend's page loads them. gzip is Bun's default level.

| backend | `.wasm` | `.wasm` gzip | JS | JS gzip | total gzip | total gzip, no custom sections |
|---|---|---|---|---|---|---|
| `@moq/net` (JS) | - | - | 386,552 | 109,486 | 109,486 | 109,486 |
| moq-wasm, wasm-bindgen | 2,194,698 | 730,348 | 39,052 | 7,782 | 738,130 | 737,871 |
| moq-ffi, UniFFI | 6,408,963 | 1,903,175 | 239,899 | 34,751 | 1,937,926 | 1,702,676 |
| slice, UniFFI | 4,527,125 | 953,899 | 78,671 | 18,675 | 972,574 | 747,969 |
| slice, BoltFFI | 4,426,230 | 924,626 | 65,177 | 14,705 | 939,331 | 721,303 |

The JS is the generated bindings plus each generator's runtime, minified: `@ubjs/core`,
`@ubjs/wasm`, and wasm-bindgen's glue for UniFFI, `@boltffi/runtime` for BoltFFI. ubrn
and BoltFFI both stage the module with its `name` section, which wasm-bindgen drops for
moq-wasm; stripping it is worth about 230 KB gzip.

The slice, which consumes and publishes over WebTransport, lands within 3% of moq-wasm
stripped under either generator. Stripped, BoltFFI is 27 KB gzip smaller than UniFFI on
the same Rust. moq-ffi's size is its surface, not its generator. Its code section (3.79 MB) by
crate, attributing a generic to the crate its path starts with:

| crate | share | crate | share |
|---|---|---|---|
| moq\_net | 25.6% | regex\_automata + regex\_syntax + aho\_corasick | 8.6% |
| alloc | 9.5% | UniFFI exports + uniffi\_core | 7.1% |
| mp4\_atom | 8.8% | moq\_json + serde\_json | 4.8% |
| core | 8.4% | moq\_ffi | 1.9% |
| moq\_mux | 8.0% | everything else | 17.3% |

The regex crates come from `hang`. mp4\_atom, moq\_mux, the regex crates, and the JSON
crates are about 30% of the code and serve the media layer; a net-only cargo feature could
drop most of them. Toggling `default-features` changes nothing on wasm32.

## Startup, CPU, and memory

The baseline sweep: the JS publisher page serves N broadcasts, and the measured page
consumes them with each backend. CPU is the consumer renderer's CDP thread time, including
connection setup, protocol, boundary copies, and byte checks, per frame received.
32 frames per subscriber. This sweep ran at load average 29 to 32.

| | JS | moq-wasm | moq-ffi, UniFFI | slice, UniFFI | slice, BoltFFI |
|---|---|---|---|---|---|
| module import and init, ms | 48 (28-94) | 39 (20-74) | 92 (64-178) | 50 (35-79) | 35 (27-106) |
| first frame, ms | 120 (82-215) | 134 (70-234) | 180 (116-332) | 126 (91-223) | 92 (72-267) |
| wasm memory before reads, MiB | - | 1.31 | 1.81 | 1.38 | 1.25 |
| JS heap after reads, MiB | 3.43 | 2.06 | 3.65 | 2.48 | 2.15 |

Startup rows are the 160 B 1x1 shape on all three relays.

CPU per frame, ms, publishers x subscribers:

| protocol, shape | JS | moq-wasm | moq-ffi, UniFFI | slice, UniFFI | slice, BoltFFI |
|---|---|---|---|---|---|
| lite-06, 160 B, 1x1 | 2.84 (2.58-2.90) | 3.96 (3.90-4.39) | 5.79 (5.56-5.98) | 4.28 (3.91-5.22) | 3.75 (3.75-4.32) |
| lite-06, 160 B, 4x4 | 0.39 (0.36-0.41) | 0.47 (0.43-0.53) | 0.58 (0.44-0.60) | 0.52 (0.38-0.54) | 0.43 (0.37-0.48) |
| lite-06, 4 KiB, 1x4 | 1.22 (1.13-1.23) | 1.53 (1.39-1.54) | 2.01 (1.83-2.06) | 1.97 (1.81-2.09) | 1.73 (1.38-1.85) |
| lite-06, 128 KiB, 1x1 | 2.03 (1.98-2.11) | 3.07 (2.95-3.23) | 4.45 (4.36-4.71) | 4.17 (3.73-4.19) | 3.26 (3.20-3.29) |
| lite-06, 128 KiB, 1x4 | 0.98 (0.97-1.08) | 1.35 (1.28-1.42) | 2.31 (2.24-2.33) | 2.34 (2.12-2.37) | 1.45 (1.41-1.57) |
| lite-06, 128 KiB, 4x4 | 1.26 (0.82-1.29) | 1.48 (1.06-1.58) | 2.17 (1.54-2.50) | 1.70 (1.61-2.08) | 1.11 (1.02-1.81) |
| transport-19, 160 B, 1x1 | 2.21 (1.67-2.43) | 2.92 (2.53-3.65) | 4.69 (3.28-4.79) | 3.28 (2.99-3.92) | 2.58 (2.45-3.92) |
| transport-19, 128 KiB, 1x4 | 1.77 (1.52-1.96) | 2.44 (2.33-2.69) | 4.19 (3.58-4.26) | 4.13 (2.94-4.66) | 2.47 (2.19-3.39) |
| lite-02, 160 B, 4x1 | 0.51 (0.49-0.52) | 0.64 (0.62-0.68) | 0.90 (0.87-0.91) | 0.75 (0.67-1.04) | 0.61 (0.58-0.65) |
| lite-02, 128 KiB, 1x4 | 1.25 (1.22-1.26) | 1.99 (1.61-2.03) | 3.44 (2.37-3.71) | 3.66 (2.46-3.91) | 2.15 (1.48-3.02) |
| lite-02, 128 KiB, 4x4 | 0.87 (0.85-1.12) | 1.11 (1.05-1.63) | 1.54 (1.53-1.62) | 1.53 (1.46-1.74) | 1.08 (1.00-1.08) |

Geometric means of the per-shape median ratios, over all 45 WebTransport shapes:

| ratio | all | 160 B and 4 KiB | 128 KiB |
|---|---|---|---|
| slice: BoltFFI / UniFFI | 0.86 | 0.90 | 0.75 |
| slice, BoltFFI / moq-wasm | 1.01 | 0.99 | 1.05 |
| slice, UniFFI / moq-wasm | 1.18 | 1.10 | 1.41 |
| moq-ffi, UniFFI / moq-wasm | 1.34 | 1.28 | 1.48 |
| moq-ffi, UniFFI / slice, UniFFI | 1.13 | 1.17 | 1.05 |

On identical Rust, BoltFFI costs 10% less CPU per frame than UniFFI for small frames and
25% less for 128 KiB ones, where UniFFI copies each payload byte by byte. Through BoltFFI,
the slice matches moq-wasm. The last row is moq-ffi's own bookkeeping over the slice's, not
the generator's. The raw samples for every shape, including the u32 and near-2^53
sequences, are in the run's `backend.json`.

## Per-call cost

Local calls, no network, so only the binding and moq-net's model count. Microseconds per
call, from one session at load average 12 to 20.

The slice, six ABBA rounds:

| call | UniFFI | BoltFFI |
|---|---|---|
| sync `noop()` | 0.57 (0.38-0.68) | 0.03 (0.02-0.05) |
| sync getter `sequence()` | 0.40 (0.35-0.55) | 0.04 (0.01-0.07) |
| async `noop_async()` | 3.07 (2.82-4.18) | 1.44 (1.10-1.98) |
| async `yield_once()`: one wakeup | 3.71 (3.16-5.28) | 2.60 (2.16-2.80) |
| `write_frame` 160 B | 2.40 (2.23-3.07) | 0.85 (0.79-0.89) |
| `write_frame` 4 KiB | 29.2 (27.9-30.9) | 2.58 (2.45-2.93) |
| `write_frame` 128 KiB | 894 (854-936) | 65.3 (62.5-70.5) |
| `read_frame`, buffered, 160 B | 6.27 (5.81-7.20) | 2.87 (2.51-4.00) |
| `read_frame`, buffered, 4 KiB | 39.6 (37.5-40.8) | 9.56 (9.05-10.6) |
| `read_frame`, buffered, 128 KiB | 1,079 (1,010-1,173) | 175 (160-203) |
| `read_frame`, parked then woken, 160 B | 9.04 (7.74-10.9) | 5.16 (3.76-6.42) |

Both generators resolve one JS Promise per `rust_future_poll` wakeup; BoltFFI's async
path is 1.4 to 2.1 times cheaper. UniFFI's bytes cost about 7 ns per byte written and 8 ns
per byte read, from its per-element `Vec<u8>` lift and lower in Rust, against BoltFFI's
bulk copies at 0.5 and 1.3. That is [Payload bytes](https://github.com/moq-dev/rust2/blob/main/quest/m1/ffi-bytes.md)'s target.
`@ubjs/core` also arms and clears a `setTimeout` on every async call, a Node.js keep-alive
that runs in the browser too.

moq-ffi itself through UniFFI, in the same session, in each of ubrn's dispatch modes (see
the strict CSP below):

| call | `new Function` dispatch | interpreted (strict CSP) |
|---|---|---|
| module init | 53.7 ms | 51.7 ms |
| sync getter `sequence()` | 0.37 | 1.03 |
| `write_frame` 160 B | 2.76 | 3.39 |
| `write_frame` 128 KiB | 922 | 951 |
| `read_frame`, buffered, 160 B | 7.10 | 8.18 |
| `read_frame`, parked then woken, 160 B | 10.5 | 11.2 |

## Parity

[`uniffi/cases.ts`](uniffi/cases.ts) and the removed `slice/cases.ts` ran against
all three relays, normally and under a strict CSP. Every case passed in both modes for all
three bindings. moq-wasm's column is the existing suite in [`src/main.ts`](src/main.ts).

| case | moq-wasm | moq-ffi, UniFFI | slice, UniFFI and BoltFFI |
|---|---|---|---|
| negotiates each relay's version | yes, via `version()` | yes, by offering only that version (the spike predates `MoqConnectionStats.version`, [#5723](https://github.com/moq-dev/moq/pull/5723)) | same as moq-ffi |
| reads a published broadcast, every byte, 128 KiB frames | yes | yes | yes |
| refuses a missing track | yes | yes: rejects with `NotFound` (transport-19) or `Protocol` (lite) | yes: rejects with the slice's `Net { reason: "not found" }` |
| publishes | no: consume only | yes | yes |
| frame timestamps | no: payload only | yes, exact, as `bigint` microseconds on a version that delivers them (not lite-02 or lite-06) | yes |
| u64 sequence above 2^53, end to end | not testable: no publisher can send it | yes, 2^53+1 on all three; 2^62-1 on lite-02 and transport-19 (see bugs) | same as moq-ffi |
| cancellation through `AbortSignal` | no: `free()` rejects pending calls | rejects with `AbortError`; the next call still gets the next frame | UniFFI: `AbortError`; BoltFFI: `BoltFFICancelledError`; neither steals the next frame |
| aborting releases the call's state | n/a | yes: a track's demand drops only once the aborted future is freed | not testable: the slice has no demand |
| freeing a handle mid-call | rejects the call | the call keeps running (each call clones the `Arc`); `cancel()` is the terminal | the call keeps running under both (BoltFFI's `dispose()`) |
| close and error propagation | yes | session `cancel()` rejects a pending remote read with `Transport` | yes, with the browser's `WebTransportError` in the reason |
| forced wasm memory growth mid-read | yes | yes | yes |
| WebSocket fallback | no | no: moq-ffi's wasm client has none | no |

Things neither generator can do: a `JsValue` cannot cross, so Rust constructs the
WebTransport, as moq-ffi already does on wasm. There are no streams; reads are one Promise
per call. UniFFI's generated `*Like` return types omit `uniffiDestroy`, so a caller casts
to the class to free a handle early; BoltFFI's classes expose `dispose()` directly. Both
fall back to a `FinalizationRegistry`. BoltFFI types an async constructor's result as
nullable (`Promise<MoqBrowserSession | null>`, [boltffi#95](https://github.com/boltffi/boltffi/issues/95)).

## Deployment

- **Strict CSP.** ubrn compiles a dispatcher per export with `new Function`. Under
  `script-src 'self' 'wasm-unsafe-eval'` it falls back to an interpreter; the harness
  checks `new Function` really is refused there. Sync calls cost up to 2.8 times as much
  interpreted; frame calls barely move. BoltFFI's cases pass under the same CSP.
- **Bundlers.** ubrn's `uniffiInitAsync` and BoltFFI's `init` both take the `.wasm` from
  the host. ubrn's `.wasm` and its wasm-bindgen `_bg.js` are a matched pair. Both generated
  entrypoints import an npm runtime (`@ubjs/wasm`, `@boltffi/runtime`).
- **Types.** ubrn's generated TypeScript carries `@ts-nocheck`, so
  [ubrn#472](https://github.com/jhugman/uniffi-bindgen-react-native/issues/472) does not
  surface. BoltFFI emits `.d.ts` files. The harness type-checks its callers against both
  under the repo's strict config.
- **Version.** ubrn pins UniFFI 0.31. Adoption waits on
  [ubrn#468](https://github.com/jhugman/uniffi-bindgen-react-native/pull/468) (0.32), or
  on carrying ubrn's TypeScript generator in [the fork](/quest/m2/uniffi-fork.md). BoltFFI
  0.31.1 is current but breaks across minor releases.
- **JS only.** ubrn's player cannot drive wasm2js output as is: it compiles small wasm
  trampolines for callbacks and the panic log, and grows `__indirect_function_table`. Its
  module class accepts any instance-shaped object, so plain calls might run through a shim.
  Not attempted for either generator; an input to the
  [WASM-to-JS evaluation](/quest/m2/rs2ts-wasm2js.md).

## What blocks BoltFFI

BoltFFI cannot bind moq-ffi without a rewrite of its public API. Adding the slice's browser
client hit two new walls, and a probe crate confirmed which moq-ffi shapes fail. Together
with [#5736](https://github.com/moq-dev/moq/pull/5736)'s list:

| blocker | moq-ffi's exposure | what the slice did | to bind moq-ffi |
|---|---|---|---|
| Any `Arc<T>`, returned or in a record, fails the whole crate's lowering ("opaque Rust pointer container cannot be represented in binding IR yet") | 56 exports return `Arc<T>` | classes by value | every object becomes a cheap-clone wrapper around its own `Arc`, M |
| Objects inside records fail too ("unsupported expansion: codec node") | `MoqClientConfig` and `MoqServerConfig` carry origins; two media records carry a timebase: 6 fields | origins live in the session | **public API break** in every language: move those objects to constructor arguments or setters, M |
| Async exports must return `Send` futures, on wasm32 too (`boltffi_core::rust_future_new`); no single-threaded mode | every browser-touching async export; WebTransport handles are `Rc` | wrapped each future in an asserted-`Send` adapter | the same wrapper on every async export, S; or a `Send`-free path in a fork, S |
| Classes must be `Send + Sync` on wasm | 47 objects | `unsafe impl` per class (from #5736) | the same, S |
| The scanner ignores target `cfg`, and binding metadata comes from a host build, so wasm-only code must compile natively | 82 `target_arch` lines | a `browser` feature, with a host stub that errors | convert the wasm gating to features with host stubs, M |
| Markers cannot sit in `cfg_attr`, and `#[error]` collides with thiserror | `MoqError` derives thiserror; all 63 export blocks, 45 records, 16 enums | shim crate (from #5736) | dual annotations through a shim, or BoltFFI only, M to L |
| No Go or C++, partial Dart and C (#5736) | 4 of moq-ffi's 8 targets | n/a | keep UniFFI for those targets and carry both generators, or write the backends, XL |

Shapes that do bind, from the probe: field defaults (`#[boltffi::default]`, moq-ffi has
97\), `HashMap` as `Map`, `Option<Object>` returns, sync and async, and error enums that
carry records.

So binding moq-ffi through BoltFFI is an L to XL project with a breaking API change. The
cheapest form is browser-only: BoltFFI for TypeScript and UniFFI for the other seven
targets, over one dual-annotated crate.

## Bugs found

- **moq-ffi panicked creating any broadcast in the browser.** `moq_mux`'s catalog clock
  called `std::time::SystemTime::now`, which wasm32-unknown-unknown does not implement.
  The measurements above used a workaround in the pin patch. Since
  [#5681](https://github.com/moq-dev/moq/pull/5681), moq-mux reads moq-clock's browser
  wall clock instead, and the cases pass without the workaround.
- **On moq-lite-06, group sequence 2^62-1 never arrived.** The cases also stamped it as
  the frame timestamp, which lite-05/06 cannot zigzag as a group's first frame, so the
  publisher dropped the group and the subscriber waited forever. Both moq-ffi and the
  slice hit it under both generators. Timestamps now stop at 2^61-1, so `writeFrame`
  refuses the unencodable one and the sequence itself crosses every version.
