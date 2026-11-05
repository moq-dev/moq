# [M] Evaluate WASM-to-JS for @moq/net

## Goal

A reproducible report evaluates JS-only output from the production Rust
browser implementation and settles the next route with the maintainer.
Try Binaryen wasm2js first; source translation is an alternative if its
measured cost is justified. The report updates the
[JS-only adoption quest](/quest/m2/rs2ts-js.md), rather than shipping an
unvalidated replacement or treating WASM as the final destination.

## Plan

Decided 2026-10-31 (maintainer): moq's WASM support (`rs/moq-wasm`,
`@moq/wasm`, moq-ffi's wasm32 build, and the browser backend baseline) is
removed, since nobody used it and it is being replaced. There is no production
WASM backend to follow or compare against. wasm2js needs a wasm32 build of the
Rust implementation again; confirm that route with the maintainer before
restoring one, or start from a source translator. Compare against the
handwritten `@moq/net`, measuring complete artifacts and matching startup paths. Require full-width u64,
lifetime, cancellation, backpressure, malformed-input, and memory-growth
coverage; the limited fixtures in #5337 do not prove these generally.

[Binaryen wasm2js](https://github.com/WebAssembly/binaryen#wasm2js) emits JS,
but retains linear-memory machinery and documents differences in trapping
and numeric conversions. Validate the reachable behavior of our build rather
than assuming Rust compilation alone proves equivalence. Pin compatible
Rust, wasm-bindgen, and Binaryen versions; check feature lowering, import
bindings, legalized i64 interfaces, and cached views after memory growth.
The attachment's bindgen text patches are prototype evidence, not a stable
integration contract. Resolve failures at the tool or binding source.

Options to retain in the report:

| Route | Why consider it | Cost to establish |
|---|---|---|
| Existing Rust -> WASM -> JS | Reuses production Rust, including async, and its tests | Size, startup, linear memory, tool compatibility, and semantic differences |
| Coarser Rust/browser boundary | Can reduce crossings, copies, and Promise work for WASM and WASM-to-JS | Measure latency/backpressure as well as throughput; no assumed win |
| Charon MIR/LLBC -> TS | Control JS representations and preserve Rust ownership semantics | A custom emitter/runtime, extraction coverage, source maps, and pinned toolchain |
| rust-js | Reuse an existing Rust-to-JS compiler and runtime | Audit its supported subset and semantic differences, especially eager Promise execution versus lazy Rust futures |
| Component Model + jco | Reusable typed binding and distribution machinery | Additional interface/adapters; default output still uses WASM, while `--js` offers JS-only output |
| Own MIR/THIR backend | Control the extraction and lowering choices; under research in moq-dev/rust2 | Highest direct ownership of compiler integration and runtime semantics |

Sources: [Charon](https://github.com/AeneasVerif/charon),
[rust-js](https://github.com/rust-js-lang/rust-js), and
[jco](https://bytecodealliance.github.io/jco/transpiling.html).
Jco is an interface/distribution alternative, not evidence of faster output.
A JS-only result need not be readable TypeScript.

If wasm2js is acceptable, scope its adoption. If not, record the concrete
reason and plan the smallest source-translator investigation that addresses
it. Do not require a new repo, a shared npm runtime, a particular integer
representation, or codec/model/session rewrites before this evidence exists.
Split independently landable prerequisites into quests.

Decided with the maintainer on 2026-10-23: an own rustc-driver translator is
developed as a parallel research track in
[moq-dev/rust2](https://github.com/moq-dev/rust2), starting with moq-net's
codec ([its quest](https://github.com/moq-dev/rust2/blob/main/quest/m2/ts.md)).
It does not replace wasm2js as the first route here. Once it translates
reachable moq-net code, compare it as the source-translator row below; moq
consumes it as a pinned flake input, never by vendoring.

Preserve earlier source-translation evidence:

- [#5049](https://github.com/moq-dev/moq/pull/5049), summarized in
  [#5341](https://github.com/moq-dev/moq/pull/5341), used a fail-closed Python
  LLBC emitter on Charon 0.1.284/nightly-2026-09-17. Real zigzag/unzigzag
  matched native Rust over 15,900 cases; the integer entry point was 840 B
  gzip. Bun timings were a browser proxy. Ownership probes were handwritten,
  not proof of general copy/drop lowering; varint::size was extracted but
  its remaining constructs were not translated.
- [#5337](https://github.com/moq-dev/moq/issues/5337) reports closure-body
  retention and control-flow duplication fixes in Charon
  [#1513](https://github.com/AeneasVerif/charon/pull/1513) and
  [#1514](https://github.com/AeneasVerif/charon/pull/1514). Both merged
  upstream; pin a Charon revision carrying them before relying on them; its emitter benchmark does not establish
  the performance of a future emitter.
- Source translation must establish precise Drop/Arc/Weak behavior, kio
  wakers and guards, unsafe storage, nested Option states, trait objects,
  value-keyed and ordered collections, and exact integer/overflow semantics.
  Codec success alone cannot establish model/session feasibility. Keep any
  proposed safe-storage or sans-IO rewrite measured and separately justified.

Public API: none; the report scopes any later changes. Wire: none.
