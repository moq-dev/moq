# [L] @moq/net runs the Rust model in the browser

## Goal

`@moq/net` ships the Rust protocol and model implementation to browsers, Node,
Bun, and Deno through rust2's WebAssembly export, with a generated TypeScript
binding. Publishing and consumption, lite and moq-transport, and supported
browser transports retain parity, and the public TypeScript interface is
preserved where practical. Translation to plain TypeScript replaces the
WebAssembly build only for a crate where it measures better.

## Plan

Decided with the maintainer on 2026-11-09, in rust2's quest audit
([rust2-wasm](https://github.com/kixel-ai/rust2/blob/main/quest/m2/wasm/README.md)):
✅ WASM ships, and TypeScript must beat it (rejected: JS-only staying the
goal with WebAssembly only a baseline; pausing the translator until
WebAssembly is measured). This supersedes the 2026-10-12 decision here that
WASM is transitional and JS-only is the final goal. Recorded here 2026-11-12
(/quest-audit); the wasm2js evaluation and the separate browser report quests
were deleted, since rust2-wasm's gate replaces both: one report compares the
same `pub` API through rust2-wasm, through rust2-ts's translation, and
hand-written `@moq/net`, on CPU time, shipped bytes, and startup, in Chrome
and Firefox, and the maintainer decides from it.

Still decided 2026-10-31 (maintainer): moq's old wasm-bindgen support stays
removed; rust2-wasm is the only WebAssembly route.

Decided with the maintainer on 2026-11-21: ✅ no hand-written `u64` type for
`@moq/net` (rejected: profiling bigint's share first; a range-checked branded
bigint; porting rust2-ts's number-or-limbs `u64` by hand). Either route here
brings rust2's `u64`, a `number` while a safe integer and `{ hi, lo }` limbs
past 2^53, which rust2's bench measured faster than `bigint` on the codec,
while a `{ hi, lo }` pair for every value allocated as a `bigint` does and lost
decode. Porting it by hand would break the bigint group IDs in the public API
for code this quest may retire. The gate's report includes the codec's integer
cost, so the comparison shows what `bigint` costs the handwritten package.

- Adopt only after the gate's report, against the handwritten `@moq/net` in a
  complete browser workload. Preserve wire and correctness, full-width
  integer values, and explicit lifetime and cancellation behavior. Run the
  browser suite and `just test interop --all` in CI.
- Pin the generation toolchain and verify reproducible package artifacts.
  Normal JS contributors and consumers should not need that toolchain. Update
  API docs and package READMEs for loading, deployment, and regeneration; any
  necessary public API break is proposed separately and migrates consumers.
- Retire the handwritten implementation only after parity and adoption.
  Preserve shared browser tests.

Public API: `@moq/net` remains; enumerate any approved changes. Wire: none.

## Waiting

- kixel-ai/rust2: its rust2-wasm epic reaches its gate, moq-net running in a browser with the three-way report. Check that rust2's `quest/m2/wasm/README.md` has no children left and its report exists.

## Related

- [Keep moq-net's model in rust2-ts's subset](/quest/m5/rs2ts-model-subset.md) - keeps the translation route measurable
