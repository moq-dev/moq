# Rust-backed @moq/net

## Goal

moq-net is the single implementation of the browser's MoQ protocol and model
layer, covering publishing and consumption for lite and moq-transport.
`@moq/net` remains the public package. Ship a production WASM replacement
first; the eventual browser distribution is JS-only, built from Rust.
Hand-written TypeScript provides browser integration and the public interface.

This m1 line delivers the WASM replacement. The
[JS-only stage](/quest/m2/rs2ts-js.md) follows in m2.

## Plan

Decided with the maintainer on 2026-10-12:

- Ship WASM in production, not only as a comparison. It is transitional;
  stopping permanently at WASM does not meet the final JS-only goal.
- Try WASM-to-JS first using the production Rust implementation and browser
  fixtures. Pursue Charon, rust-js, or another source translator only if
  measured costs justify it. These are alternatives, not mandatory successive
  implementations. A new generic translator repo is not a prerequisite.
- Allow explicitly reviewed bundle-size, startup, and CPU tradeoffs for
  reduced maintenance. Compare complete browser workloads before each
  adoption and record the maintainer's decision; do not invent percentage
  thresholds. Correctness and wire compatibility remain required.
- Keep the TypeScript interface of `@moq/net` where practical. Hide backend
  initialization and ownership details where feasible, without duplicating
  protocol or model logic. Propose necessary API breaks separately and migrate
  in-tree consumers with the change; do not expose Rust bindings by default.
- Update existing API docs and package READMEs, including loading, deployment,
  and regeneration instructions. No separate browser-backend guide quest.
- Continue fixing handwritten js/net until adoption. Retain its identified
  baseline and reusable tests when deleting the replaced implementation.

This supersedes the earlier generated-TypeScript-only decision, the absolute
no-regression gate, and the codec/model/session translation sequence. Keep
Rust async, kio, integer representations, and storage unless evidence calls
for a source change. A batched boundary is an optimization candidate, not a
reason to require a full sans-IO rewrite. Controlled time remains independently
useful; source-level `Instant` mappings depend on the chosen backend.

Evidence from [#5337](https://github.com/moq-dev/moq/issues/5337) motivates
reopening the options, not choosing a winner. Its attachment reports the
original async Rust consumer passing nine browser tests in both WASM and
WASM-to-JS, without Rust edits, at about 543 KB and 875 KB gzip respectively.
A separate synchronous publisher prototype reports median times of about
8.6 us/frame for WASM, 25.1 for WASM-to-JS, and 100.9 for its Charon emitter.
The issue's 27.9 versus 104 us comparison is a different minified run.
These are contributor measurements, not reproduced results. The publisher
comparison is not fully reproducible from the attachment, excludes network
and encryption, and has incomparable startup paths. The async fixtures are
consume-only and do not cover values above the JS exact-integer range.

Public API: the plan retains `@moq/net`; implementation quests must enumerate
any necessary breaks. Wire: no changes intended.

## Required

- [Browser backend baseline](/quest/m1/rs2ts/spike.md) - reproducible browser fixtures and measurements before choosing or adopting a backend
- [Complete WASM bindings](/quest/m1/rs2ts/wasm-bindings.md) - Rust publishing and consumption cover the browser protocol and model surface
- [Adopt WASM in @moq/net](/quest/m1/rs2ts/wasm-adopt.md) - the TypeScript interface uses Rust, with measured tradeoffs accepted before replacement

## Related

- [WASM-to-JS evaluation](/quest/m2/rs2ts-wasm2js.md) - the first JS-only candidate, with source translators as alternatives
- [JS-only @moq/net](/quest/m2/rs2ts-js.md) - the eventual distribution uses no WASM runtime
- [Browser media report](/quest/m2/rs2ts-browser-report.md) - broader media measurements after adoption
- [Controlled time](/quest/m1/time/README.md) - coherent Rust and JS clocks regardless of backend
