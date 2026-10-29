# [XS] Validate private JS constructors

## Goal

Complete local validation of the private-constructor/module-factory implementation
for Rendition, Lister, Arrivals, and both E2EE generation roles, then mark the PR
ready for review. No package export exposes their internal factories. Wire stays
unchanged.

## Plan

- The implementation and author review are complete. Declaration fixtures cover
  all five roles, valid factory consumers, and factory export boundaries. The
  shared package build checks source versus emitted constructor visibility and
  arity, including private packages. Biome 2.5.15 GritQL matched constructors but
  could not inspect their leading JSDoc; this uses the settled declaration fallback.
- Remaining: finish `just ci` locally, including current-consumer type checks and
  runtime tests. The first attempt failed before JS validation because the host's
  mbx Cargo wrapper reported `Argument list too long`. The retry using the pinned
  Nix Cargo directly could not pass `quest gate` host admission and was stopped.
  Run with the pinned Nix toolchain first on PATH and BASH_ENV unset so nested
  shells do not reinsert the wrapper; keep the resource gate.
- Passed: all workspace builds; the affected net/publish/watch API reports; all
  four built-declaration fixture projects; the shared guard's unit tests and type
  check; a fresh stripped-constructor mutation that the guard rejects; runtime
  checks that package entrypoints do not export factories; quest validation.
- Keep the PR draft until the remaining local gate is complete. Delete this quest
  and its links when validation is complete. Do not merge or enqueue this attempt.

Public API: breaking construction of Rendition migrates to `broadcast.video`,
`audio`, or `text`. Internal handles come from the broadcast wire view,
`Audio.Decoder.arrivals()`, or `Credential.produce()` / `consume(epoch)`.
