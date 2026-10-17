# Browser binding generator report

Which Rust binding `@moq/net` wraps in the browser: moq-ffi through UniFFI (ubrn's
`wasm2` player), the moq-ffi slice through BoltFFI, or the hand-written wasm-bindgen
`rs/moq-wasm`. This compares them on the [baseline harness](README.md#backend-baseline).
It adopts nothing.

Decision: open, see the PR that added this file.

## Reproduce

```bash
just test wasm-baseline --uniffi   # moq-ffi over ubrn: parity cases, then the baseline sweep with a third backend
just test wasm --slice             # the slice's per-call costs under UniFFI and BoltFFI (needs bench/ffi-gen, #5736)
bun test/wasm/uniffi/size.ts <module.wasm>...   # sections and per-crate code share
```

ubrn 0.31.0-6 reads UniFFI 0.31 metadata and moq-ffi is on 0.32, so
[`uniffi/build.sh`](uniffi/build.sh) applies [`uniffi/pin.patch`](uniffi/pin.patch) to a
scratch checkout. The pin is measurement-only and does not land. The patch also carries a
browser fix moq-ffi needs (see [Bugs found](#bugs-found)). moq-ffi built on 0.31 with
no other source change. [`slice/build.sh`](slice/build.sh) does the same to the slice.

## Setup

Measured 2026-10-16 at `3789ee2ef` on an AMD Ryzen 9 5900XT (32 threads), Linux,
headless Chromium 153.0.8010.12. The host was shared with other agents (load average
13 to 25 during the sweep), so treat absolute numbers as noisy. Every comparison
interleaves its candidates: the sweep runs js, js, wasm, wasm, uniffi, uniffi, js, wasm,
uniffi, uniffi, wasm, js per shape, and the slice runs ABBA rounds on fresh pages. Cells
are medians with the min to max range.

All builds use `wasm-release` (opt-level z, fat LTO) and no wasm-opt, as moq-wasm ships.

## Size

Shipped assets, as each backend's page loads them. gzip is Bun's default level.

| backend | `.wasm` | `.wasm` gzip | JS | JS gzip | total gzip |
|---|---|---|---|---|---|
| `@moq/net` (JS) | - | - | 386,552 | 109,486 | 109,486 |
| moq-wasm, wasm-bindgen | 2,194,698 | 730,348 | 39,052 | 7,782 | 738,130 |
| moq-ffi, UniFFI | 6,408,963 | 1,903,175 | 239,899 | 34,751 | 1,937,926 |
| moq-ffi, UniFFI, no custom sections | 4,533,099 | 1,667,925 | 239,899 | 34,751 | 1,702,676 |
| slice, UniFFI | 1,278,308 | 257,617 | 65,628 | 15,853 | 273,470 |
| slice, BoltFFI | 1,175,797 | 231,257 | 53,761 | 11,987 | 243,244 |

UniFFI's JS is the generated bindings, `@ubjs/core` and `@ubjs/wasm`, and wasm-bindgen's
glue, minified. ubrn stages the module with its `name` section; wasm-bindgen drops it for
moq-wasm, which is worth 235 KB gzip.

moq-ffi binds the whole surface, so the `.wasm` is 2.3 times moq-wasm even stripped. Its
code section (3.79 MB) by crate, attributing a generic to the crate its path starts with:

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
32 frames per subscriber.

| | JS | moq-wasm | UniFFI |
|---|---|---|---|
| module import and init | 29 ms (26-33) | 21 ms (18-24) | 61 ms (58-69) |
| first frame | 74 ms (60-79) | 79 ms (68-87) | 118 ms (104-130) |
| wasm memory before reads | - | 1.31 MiB | 1.81 MiB |
| JS heap after reads | 3.33 MiB | 2.06 MiB | 3.62 MiB |

Startup rows are the 160 B 1x1 shape on all three relays.

CPU per frame, ms, publishers x subscribers:

| protocol, shape | JS | moq-wasm | UniFFI |
|---|---|---|---|
| lite-06, 160 B, 1x1 | 1.56 (1.47-1.70) | 2.33 (2.17-2.44) | 3.13 (3.06-3.37) |
| lite-06, 160 B, 4x4 | 0.21 (0.21-0.22) | 0.26 (0.25-0.26) | 0.31 (0.29-0.31) |
| lite-06, 4 KiB, 1x4 | 0.57 (0.54-0.61) | 0.73 (0.72-0.76) | 0.97 (0.95-0.98) |
| lite-06, 128 KiB, 1x1 | 2.09 (2.03-2.14) | 3.32 (3.15-3.54) | 4.67 (4.63-4.74) |
| lite-06, 128 KiB, 1x4 | 0.97 (0.93-0.97) | 1.28 (1.23-1.31) | 2.03 (1.96-2.07) |
| lite-06, 128 KiB, 4x4 | 1.44 (0.87-1.65) | 1.69 (1.18-2.17) | 2.23 (1.84-2.77) |
| transport-19, 128 KiB, 1x4 | 1.24 (1.07-1.67) | 1.48 (1.41-1.68) | 2.20 (2.06-2.26) |
| lite-02, 128 KiB, 1x4 | 1.03 (0.91-1.16) | 1.29 (1.27-1.35) | 2.11 (2.06-2.22) |

The raw samples for every shape, including the u32 and near-2^53 sequences, are in the
run's `backend.json`. UniFFI costs 1.2 to 1.6 times moq-wasm's CPU per frame here, more
as frames grow, since every payload crosses as a record copied byte by byte.

## Per-call cost

Local calls, no network, so only the binding and moq-net's model count. Microseconds per
call.

The slice, six ABBA rounds:

| call | UniFFI | BoltFFI |
|---|---|---|
| sync `noop()` | 0.65 (0.33-0.74) | 0.03 (0.02-0.04) |
| sync getter `sequence()` | 0.33 (0.27-0.38) | 0.04 (0.01-0.06) |
| async `noop_async()` | 2.33 (2.28-2.62) | 1.50 (1.00-1.74) |
| async `yield_once()`: one wakeup | 3.11 (3.00-3.62) | 1.79 (1.64-2.04) |
| `write_frame` 160 B | 1.64 (1.58-1.73) | 0.76 (0.72-0.85) |
| `write_frame` 4 KiB | 13.6 (13.3-13.8) | 2.26 (2.04-2.35) |
| `write_frame` 128 KiB | 401 (395-420) | 58.5 (51.8-63.2) |
| `read_frame`, buffered, 160 B | 5.47 (5.29-5.69) | 2.57 (2.34-3.28) |
| `read_frame`, buffered, 4 KiB | 38.5 (37.5-39.0) | 8.08 (7.07-8.62) |
| `read_frame`, buffered, 128 KiB | 1,052 (1,039-1,079) | 141 (124-146) |
| `read_frame`, parked then woken, 160 B | 7.19 (6.70-7.82) | 3.75 (3.40-4.00) |

UniFFI resolves one JS Promise per `rust_future_poll` wakeup, as BoltFFI does; the
async gap is about 2 times. Its bytes cost about 3 ns per byte written and 8 ns per byte
read, from UniFFI's per-element `Vec<u8>` lift and lower in Rust plus a JS-side copy,
against BoltFFI's bulk copies. That is [Payload bytes](/quest/m2/ffi-bytes.md)'s target.
`@ubjs/core` also arms and clears a `setTimeout` on every async call, a Node.js
keep-alive that runs in the browser too.

moq-ffi itself through UniFFI, one run per mode (the strict CSP below):

| call | `new Function` dispatch | interpreted (strict CSP) |
|---|---|---|
| module init | 52.7 ms | 46.4 ms |
| sync getter `sequence()` | 0.34 | 0.89 |
| `write_frame` 160 B | 2.39 | 3.58 |
| `write_frame` 128 KiB | 883 | 933 |
| `read_frame`, buffered, 160 B | 6.41 | 8.54 |
| `read_frame`, parked then woken, 160 B | 9.70 | 9.00 |

The slice's `sequence()` matches moq-ffi's. Compare the frame rows across these two tables
only roughly: they ran at different times on the shared host, and moq-ffi's frames carry
more bookkeeping than the slice's.

## Parity

[`uniffi/cases.ts`](uniffi/cases.ts) runs against all three relays, in both dispatch modes.
moq-wasm's column is the existing suite in [`src/main.ts`](src/main.ts).

| case | moq-wasm | moq-ffi, UniFFI |
|---|---|---|
| negotiates each relay's version | yes, via `version()` | yes, by offering only that version: moq-ffi exposes no negotiated version |
| reads a published broadcast, every byte, 128 KiB frames | yes | yes |
| refuses a missing track | yes | yes: rejects with `NotFound` (transport-19) or `Protocol` (lite) |
| publishes | no: consume only | yes |
| frame timestamps | no: payload only | yes, exact, as `bigint` microseconds |
| u64 sequence above 2^53, end to end | not testable: no publisher can send it | yes, 2^53+1 on all three; 2^62-1 on lite-02 and transport-19 (see bugs) |
| cancellation | `free()` rejects pending calls | every async method takes `{ signal }`; abort rejects with `AbortError` and the next call still gets the next frame |
| aborting releases the call's state | n/a | yes: a track's demand drops only once the aborted future is freed |
| freeing a handle mid-call | rejects the call | the call keeps running: each call clones the `Arc`; `cancel()` is the terminal |
| close and error propagation | yes | session `cancel()` rejects a pending remote read with `Transport` |
| forced wasm memory growth mid-read | yes | yes |
| WebSocket fallback | no | no: moq-ffi's wasm client has none either |

Things the binding cannot do: a `JsValue` cannot cross, so Rust constructs the
WebTransport, as moq-ffi already does on wasm. There are no streams; reads are
one Promise per call. The generated `*Like` return types omit `uniffiDestroy`, so a
caller casts to the class to free a handle early, and a `FinalizationRegistry` frees the
rest.

## Deployment

- **Strict CSP.** ubrn compiles a dispatcher per export with `new Function`. Under
  `script-src 'self' 'wasm-unsafe-eval'` it falls back to an interpreter; the harness
  checks `new Function` really is refused there. Every case passes in both modes. Sync
  calls cost up to 2.6 times as much interpreted; frame calls barely move.
- **Bundlers.** `uniffiInitAsync` takes the `.wasm` URL from the host. The `.wasm` and
  its wasm-bindgen `_bg.js` are a matched pair. The generated `index.ts` imports
  `@ubjs/wasm`, which resolves its browser build through package `exports`.
- **Types.** The generated TypeScript carries `@ts-nocheck`, so
  [ubrn#472](https://github.com/jhugman/uniffi-bindgen-react-native/issues/472) does not
  surface. The harness type-checks its callers against it under the repo's strict config.
- **Version.** ubrn pins UniFFI 0.31. Adoption waits on
  [ubrn#468](https://github.com/jhugman/uniffi-bindgen-react-native/pull/468) (0.32), or
  on carrying ubrn's TypeScript generator in [the fork](/quest/m2/uniffi-fork.md).
- **JS only.** The player cannot drive wasm2js output as is: it compiles small wasm
  trampolines for callbacks and the panic log, and grows `__indirect_function_table`. Its
  module class accepts any instance-shaped object, so plain calls might run through a shim.
  Not attempted; an input to the [WASM-to-JS evaluation](/quest/m2/rs2ts-wasm2js.md).

## BoltFFI

The slice binds and runs in Chromium under BoltFFI 0.31.1, and is the fastest on every
call. That passes the feasibility filter, but it is not selection evidence:

- BoltFFI cannot bind moq-ffi as written. [#5736](https://github.com/moq-dev/moq/pull/5736)
  lists eleven gaps, including `Arc` returns, ignored target `cfg`, and `Send + Sync`
  on wasm.
- The slice has no client on wasm, because BoltFFI's scanner ignores target `cfg` and
  web-transport-wasm is gated on it. So no network parity, size, startup, or memory
  numbers exist for it.
- \#5736 recommends forking UniFFI for the native targets. Picking BoltFFI for the browser
  alone would carry two generators.

If BoltFFI is favored anyway, the quest's next step stands: port the harness workload to it
and rerun parity, size, startup, and memory before picking it.

## Bugs found

- **moq-ffi panics creating any broadcast in the browser.** `moq_mux`'s catalog clock
  calls `std::time::SystemTime::now`, which wasm32-unknown-unknown does not implement.
  The pin patch leaves the wall clock unset in the browser, as moq-net already does for
  its own; the real fix belongs to the bindings parity work.
- **On moq-lite-06, group sequence 2^62-1 never arrives.** lite-02 and transport-19
  carry it, and 2^53+1 crosses all three. Nothing errors: the subscriber waits forever.
