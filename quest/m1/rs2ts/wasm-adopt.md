# [L] Adopt WASM in @moq/net

## Goal

`@moq/net` uses moq-net through production WASM for publishing, consumption,
lite, and moq-transport. Its replaced handwritten protocol and model code is
deleted after browser parity and measured tradeoffs are accepted.

## Plan

Decided 2026-10-12: keep `@moq/net` as the public package and preserve its
TypeScript interface where practical. Initialization and ownership belong
behind that interface where feasible; necessary breaks require a separate
maintainer decision before adoption, not an automatic Rust-shaped API.
Update watch, publish, hang, room, demos, and other affected consumers together.

- Compare the complete candidate against the retained handwritten baseline
  using the browser harness before switching the default. Include publishing,
  consumption, both protocol families, supported transports, and real
  package loading. Run `just test interop --all` and browser integration in CI.
- Present bundle size, cold startup/first-frame latency, CPU, memory, and
  usability costs. Record the maintainer's adoption decision and accepted
  tradeoffs in the PR. There is no absolute no-regression threshold, but a
  correctness or wire regression cannot be traded for maintenance savings.
- Package generated bindings and the WASM asset reproducibly so normal JS
  contributors and npm consumers do not need a Rust toolchain. Pin the build
  tools and verify release artifacts in CI; do not commit binary artifacts.
- Verify loading and deployment with supported bundlers and consumers,
  including asset URLs and applicable CSP constraints. Explain requirements
  in existing API docs and package READMEs, together with regeneration.
- Retain baseline revisions and shared tests after removing the handwritten
  implementation. Keep the WASM backend while the JS-only stage is evaluated.

Public API: `@moq/net` remains the package; enumerate any approved signature,
initialization, lifetime, or deployment changes. Wire: none.

## Required

- [Complete WASM bindings](/quest/m1/rs2ts/wasm-bindings.md) - the complete Rust-backed candidate and parity coverage
