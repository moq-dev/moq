# [S] Measure the Linux cache effect of native media exclusions

## Goal

Scheduled Linux jobs record the cache hit rate and build-time effect of
removing native media requirements from unrelated test builds. Bring a
material cache regression to the maintainer before declaring the tradeoff
settled.

## Plan

The implementation in [moq-dev/moq#7713](https://github.com/moq-dev/moq/pull/7713)
adds the derived trybuild-edge guard and excludes `pipewire`, `libspa`, and
`clang-sys` from the workspace-hack's final output. Other transitive features
remain unified. Linux x86_64 and ARM test graphs of `moq-json` no longer reach
PipeWire or libclang. An x86_64 nextest run with shared compiler caching
bypassed passes with native media pkg-config probes refused and an invalid
libclang path. The original manifests fail under the same restriction.

Local Cargo 1.98.1 unit graphs for `moq-json`, `moq-video --features pipewire`,
and `moq-audio --features pipewire`, each built with `--tests`, have 531 distinct
third-party recursive compilation identities both before and after. The JSON
build loses 30 units; video retains 501 total units. Four media identities
change. These are graph identities, not mbx hit rates or CPU measurements.

Additional Linux x86_64 unit graphs at the merged main baseline
`ac0f04e5dcca9a9a07d15135dc2e904215c494f3` compare `moq-video --features vaapi`
and `moq-video --features pipewire`, each with `--tests`. VAAPI-only drops from
478 to 456 total units; PipeWire stays at 491. Recursive third-party identities
shared by those builds drop from 453 to 428, and their union grows from 483 to
486. Excluding `clang-sys` lets VAAPI's bindgen 0.70 request `clang_6_0`, while
PipeWire's bindgen 0.72 requests `clang_11_0`; the library and its two build-script
units split into three extra compilation identities. The earlier three-build
sample does not cover this cost. Record this VAAPI/PipeWire split when interpreting
the scheduled cache results. These graphs measure compilation identities, not
CPU cost or cache hit rates; the post-merge measurements still decide whether
the tradeoff is material.

Use the same scheduled Linux job before and after this PR's merge, beginning
with `Nightly (Rust benchmark tests)`, and retain its mbx cache summaries and
available compiler-time fields. Compare the last scheduled `main` run before
the merge with the first two after it. Failed or cancelled builds and missing summaries
are incomplete evidence, not zero cache hits. Record source/build differences
that prevent attributing a change to these exclusions.

The 2026-11-02 scheduled run is a pre-merge candidate:
[run 41483654426](https://github.com/moq-dev/moq/actions/runs/41483654426),
[Rust benchmark tests](https://github.com/moq-dev/moq/actions/runs/41483654426/job/125408205554).
Its 12 mbx object-cache summaries total 986 hits, 721 misses, 323 not
looked up, 8 bypassed, and 11 remote failures. The conditional hit rate is
57.76% (`986 / (986 + 721)`); this excludes not-looked-up and bypassed units.
The summaries report estimated compiler time avoided, not measured CPU totals.
The 5 GiB download budget was exhausted during the first build; remote
failures and mixed build phases limit attribution. Replace this
candidate if a later scheduled run precedes this PR's merge.

Public API: none. Wire: none.

## Waiting

- moq-dev/moq: after the native-media exclusions merge, collect the last
  scheduled Linux baseline and first two post-merge Nightlies' mbx hit rates
  and available compiler-time fields. No post-merge evidence for these
  exclusions exists yet. If the cache cost is material, present the measured tradeoff to the
  maintainer; recommend keeping the narrow final exclusions unless the
  measurements justify another design.

## Related

- [Unify dependency features with cargo-hakari](/quest/m0/ci/hakari.md) - owns
  the separate macOS/Windows evidence and FFI invocation alignment in
  [moq-dev/moq#7666](https://github.com/moq-dev/moq/pull/7666); do not duplicate
  those measurements or depend on its unmerged implementation
