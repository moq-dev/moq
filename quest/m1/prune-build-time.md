# [M] Remove what dominates build time

## Goal

The crates, features, generics, and generated code that dominate dev,
release, and CI build time are removed, or refactored to build cheaper, with
before and after numbers recorded.

## Plan

This is a one-off, off-cycle run of the [compile time audit](/quest/m2/audit-compile-time.md),
ahead of the cut. The recurring audit keeps its schedule, and this quest
creates no successor. Its PR rewrites that audit's baselines and its "what to
check first" list with what this run learned, including whether the m2
audit's open items were done here.

Decided 2026-11-01 (maintainer):

- Default to removing. Ask whether the costly code or crate is worth keeping
  at all before making it compile faster.
- Change inline in this quest's PR, one commit per change, each with its
  measured build-time effect. Removing a public API or a user-facing feature
  is a follow-up through `/quest-plan`.

Dependencies belong to
[Remove the dependencies that don't earn their weight](/quest/m1/prune-deps.md).
This quest covers our own crates and code: monomorphization (`cargo
llvm-lines`), proc macros, build scripts, and crates or features that every
build compiles but few users need. Start from the m2 audit's baselines and
its measurement pitfalls. The [poll-first epic](/quest/m1/poll-first/README.md)
is already gating async wrappers behind a feature, so don't redo that work.
