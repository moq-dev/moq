# [S] Measure moving unit tests out of hub crates

## Goal

A report on what CI CPU time moq would save if hub crates, starting with moq-net,
stopped compiling their library twice: once as the library and once as the unit
test target, with unit tests about 52% of moq-net's `src` lines. It estimates the
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
