# [L] Ship JS-only @moq/net from Rust

## Goal

`@moq/net` ships the Rust protocol and model implementation as JavaScript
without requiring a WASM runtime. Publishing and consumption, lite and
moq-transport, and supported browser transports retain parity. The public
TypeScript interface is preserved where practical.

## Plan

Decided 2026-10-12: WASM is transitional and JS-only is the final goal.
The evaluation scopes this quest for the chosen backend and adds any required
research or implementation blockers before it becomes executable. Rejecting
wasm2js does not automatically select Charon or abandon the JS-only goal.

Reuse the production WASM and retained handwritten baselines for a complete
browser comparison before adoption. Present measured size, startup, CPU,
memory, and usability tradeoffs for the maintainer to accept; preserve wire
and correctness. Run the browser suite and `just test interop --all` in CI.
Ensure shipped output loads without WebAssembly and preserves full-width
integer values and explicit lifetime/cancellation behavior.

Pin the generation toolchain and verify reproducible package artifacts.
Normal JS contributors and consumers should not need that toolchain. Update
existing API docs and package READMEs for loading, deployment, and regeneration;
any necessary public API break is proposed separately and migrates consumers.

Retire obsolete runtime loaders, package paths, and CI only after JS-only
parity and adoption. Preserve the Rust binding crate, WASM build tools, or
WASM comparison baseline wherever generation or tests still need them;
wasm2js may still require WASM at build time. Preserve shared browser tests.
Review the linked older WASM issues against the delivered behavior and close
only those actually resolved, rather than closing them merely on deletion.

Public API: `@moq/net` remains; enumerate any approved changes and obsolete
unpublished package removals. Wire: none.

## Required

- [WASM-to-JS evaluation](/quest/m2/rs2ts-wasm2js.md) - the selected route, evidence, and any additional prerequisites

## Related

- [#2907](https://github.com/moq-dev/moq/issues/2907) - reassess against the replacement before retiring WASM paths
- [#2822](https://github.com/moq-dev/moq/issues/2822) - reassess against the replacement before retiring WASM paths
- [#2835](https://github.com/moq-dev/moq/issues/2835) - reassess against the replacement before retiring WASM paths
