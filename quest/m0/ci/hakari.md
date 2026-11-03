# [S] Record hosted cargo-hakari cache evidence

## Goal

Nightly's hosted macOS and Windows jobs record their mbx hit rate and CPU time
before and after the workspace-hack.

## Plan

Decided with the maintainer on 2026-10-25: use cargo-hakari rather than only
trimming CI's feature permutations.
[moq-dev/moq#6985](https://github.com/moq-dev/moq/pull/6985) added the path-only
dev-dependency layout, unifying test, lint, and bench builds. Keep that layout;
library-only builds do not need the hack.

The remaining FFI invocation alignment is implemented in
[moq-dev/moq#7666](https://github.com/moq-dev/moq/pull/7666): default builds use
`cargo build-ffi` with `openh264`, native builds omit `--target`, and only cross
builds name a target. Python and Dart host tests retain `testing` for the manual
clock and leak allocator, with separate target directories so they cannot
replace the default cdylib. Their third-party feature identities match the
default host build; the intentional `kio/manual` split affects workspace crates.

Compare the mbx summaries of the last scheduled Nightly before the
[2026-11-02 merge](https://github.com/moq-dev/moq/commit/8e53bf32185fec578e460ff6ccbe41a80a039a6a)
with the first two after. Query only scheduled runs on `main`:
`gh run list --workflow nightly.yml --branch main --event schedule`.
The baseline is [2026-11-01](https://github.com/moq-dev/moq/actions/runs/41313466339);
the first after is [2026-11-02](https://github.com/moq-dev/moq/actions/runs/41483654426).
The first after has no Windows compilation evidence: NASM installation failed
with a Chocolatey HTTP 503 before the build. macOS has partial mbx summaries
but its Rust suite failed. Do not infer a completed-job CPU saving from either. Separate this FFI alignment's later landing
from the workspace-hack landing when interpreting the results.

Public API and wire: none.

## Waiting

- moq-dev/moq: the second scheduled Nightly on `main` after the workspace-hack
  merge has not run yet. Once it completes, collect both platforms' before and
  first-two-after mbx summaries and CPU measurements, recording missing or
  partial evidence explicitly. A manual Nightly dispatch is not a substitute.
