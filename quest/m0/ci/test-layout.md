# [S] Measure moving unit tests out of hub crates

## Goal

A report on what CI CPU time moq would save if hub crates, starting with moq-net,
stopped compiling their library twice: once as the library and once as the unit
test target, with test-owned code covering 49.9% of moq-net's `src` lines at
main 5516a406f4. It estimates the
saving, from a prototype on moq-net, of `[lib] test = false` plus one
integration-test crate that reaches private items through a hidden module or a
feature (moq-net already has a `fuzz` feature for this). It recommends whether to
restructure, and how.

## Plan

Decided with the maintainer on 2026-10-25: measure before restructuring. It is
potentially the largest single win, since every moq-net change in CI compiles
both targets, but it changes how every test is written.

- Count how many unit tests use private items, and what a test-only module would
  have to expose.
- Measure CPU for a moq-net edit plus its tests, before and after, with mbx warm.
- Weigh the churn, and say whether moq-net is the only crate worth doing.

Public API: none, unless the hidden test surface leaks; say so if it does.

The [2026-10-26 inventory and prototype](/.github/test-layout-audit.md) records
42 library inventories, moq-net's 2,719 compiled unit cases, and an exact restored
four-case external-harness patch. The four private Delivered cases pass both
ways, but the prototype exposes a type and test getters under the public `fuzz`
feature and leaves 2,715 unit cases unmigrated. No architecture change is retained.

The lexical private-name inventory is a review starting point, not a certified
private-use count. Remaining: resolve the private/test-support surface and its
non-async feature boundary, build a complete coverage-preserving prototype, and
measure identical edit-plus-tests work with mbx warm on a quiet host. No CPU
saving is estimated from source lines or the four-case probe. Recommend keeping
the current layout until that evidence exists; moq-mux is also a substantial
structural candidate, but its CPU opportunity is unmeasured.
