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
The baseline is [2026-11-01](https://github.com/moq-dev/moq/actions/runs/41313466339).
The [2026-11-02 run](https://github.com/moq-dev/moq/actions/runs/41483654426)
has partial macOS summaries before a websocket-test timeout and no Windows
compilation (NASM installation returned HTTP 503).
[2026-11-03](https://github.com/moq-dev/moq/actions/runs/41656613559) completed
both platform jobs. [2026-11-04](https://github.com/moq-dev/moq/actions/runs/41852517544)
completed Windows, but cancelled macOS before any step ran. Windows therefore
has two complete after-runs; macOS has only one. Logs expose object-cache counts
and estimated compiler work saved, but no measured user/system CPU totals.
Do not call elapsed time CPU time, or attribute these earlier runs to the FFI
alignment that has not landed yet.

The 2026-11-04 maintainer decision accepts hosted CI as the strict Nix gate;
a local namespace-probe skip is expected. The pkg-config and Gradle repairs
have landed, so neither remains an external prerequisite.

Public API and wire: none.

## Waiting

- moq-dev/moq: inspect the next scheduled Nightly for a second complete macOS
  after-run. Recommend retaining this measurement wait; never dispatch Nightly.
- Maintainer: the existing logs contain estimated compiler work saved, not
  measured CPU totals. Accept those estimates as sufficient, or add CPU
  instrumentation before collecting future scheduled evidence. Recommend adding
  instrumentation if measured CPU remains required; do not relabel wall time.
