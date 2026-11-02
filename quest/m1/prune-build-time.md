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

Dependency removals belong to the
[dependency audit](/quest/m2/audit-deps.md).
This quest covers our own crates and code: monomorphization (`cargo
llvm-lines`), proc macros, build scripts, and crates or features that every
build compiles but few users need. Start from the m2 audit's baselines and
its measurement pitfalls. The [poll-first epic](/quest/m1/poll-first/README.md)
is already gating async wrappers behind a feature, so don't redo that work.

### Checkpoint (2026-11-01)

Claim: [moq-dev/moq#7602](https://github.com/moq-dev/moq/pull/7602).
The current measurements and rejected reader-consolidation patches are in
[the recurring audit](/quest/m2/audit-compile-time.md) and
[its raw measurements](/.github/compile-time-audit.json). No production
change is retained: changing our MP4 readers removes only ten lib IR lines,
leaves the expensive decoder variants needed by mp4-atom itself, and grows
the test-profile IR when extended to the metadata reader. Rebuild CPU and
instructions do not improve beyond noise.

The shipping OpenH264 FFI comparison is complete: fat/thin archives are
62.4/71.4 MiB and final units 135.7/88.0 s, but whole-command CPU is
390.19/547.74 s for the same 474 package/target/feature units. The 72-case
session CPU comparison is noisy (thin/fat geometric mean 1.034). Keep fat
LTO: the final-unit saving did not make the whole build cheaper, and no
runtime win justifies changing the recorded choice. This is measured, not
an unimplemented optimization proposal.

Owned crates, kio macros, platform aliases, config parsers, and binding
generators still have current consumers. No public feature removal is
proposed, and no production or profile change is retained.

Remaining: resolve validation, measure a real mbx incremental edit, and
complete local review before deleting this quest and its links. The recurring
audit already has this run's baselines and rejected candidates.

Validation is blocked: `just rs test -p moq-mux --lib` cannot launch Cargo
through mbx (E2BIG), tracked by
[moq-dev/moq#7594](https://github.com/moq-dev/moq/pull/7594). After that fix,
measure a real edit through mbx, rerun the local gate (including the known
runner-image user-namespace limitation), and complete `/quest-review`.
Do not substitute the cache-disabled measurement builds for validation.

Concurrent dependency replacements remain with
[moq-dev/moq#7601](https://github.com/moq-dev/moq/pull/7601), test pruning
with [moq-dev/moq#7600](https://github.com/moq-dev/moq/pull/7600), cross-job
feature unification with [moq-dev/moq#6985](https://github.com/moq-dev/moq/pull/6985),
and the slow-test cohort with [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987).
No successor or follow-up quest is created.
