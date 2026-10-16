# [M] Retire expensive tests with little additional signal

## Goal

The selected test suites consume less CPU while preserving useful regression
signal. Expensive redundant tests can be deleted, narrowed, or moved to a
lighter harness, with their lost coverage and measured savings made explicit.

## Plan

- Decided 2026-10-15: evidence-backed retirement is allowed. This supersedes
  the CI epic's blanket requirement that every removed PR test run elsewhere.
  Keep protocol, media correctness, refusal, and cross-language regressions
  where they add distinct signal; fewer assertions is not itself a benefit.
- Inventory runtime and CPU by selected suite before changing tools. Compare
  equivalent checks with bounded workers and realistic cold/warm caches;
  retain a replacement only when its signal and cost justify it.
- Recheck mutation/drill placement. Routine advisory sensitivity runs belong
  nightly when PR coverage adds no useful signal; retain cheap patch/harness
  validation where source changes can invalidate the drills. Some of this
  routing may already have landed, so avoid duplicating it.
- Use existing Bun and nextest capabilities before introducing another test
  framework. Report each deletion's purpose, remaining regression anchor, and
  measured cost, and keep retained tests reachable through normal CI routes.
- Do not duplicate existing platform-queue, FFI reuse, or cache work.
