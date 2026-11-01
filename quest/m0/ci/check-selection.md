# [S] The gate selects checks for every input that can break them

## Goal

`just check`'s Rust selection runs without jq errors, a change to the
pinned `rust-nightly` in `flake.nix` runs send-depth, and a change to
`rs/moq-ffi` or `test/wasm/uniffi/` type-checks `test/wasm/uniffi/` in the
merge gate, so a binding API change can no longer break Nightly's
`wasm-baseline --uniffi` silently.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7303](https://github.com/moq-dev/moq/pull/7303), [moq-dev/moq#7280](https://github.com/moq-dev/moq/pull/7280), [moq-dev/moq#7304](https://github.com/moq-dev/moq/pull/7304), and [moq-dev/moq#6946](https://github.com/moq-dev/moq/pull/6946). Milestone: ✅ m0 (user, "all recommended" for quests 1-8).

- **jq error**: #7303's `just check` logged `jq: error ... null cannot be
  matched` from the Rust selection without failing. A jq `test`/`split` on a
  null field is the usual cause; candidates are the filters in
  `sh/rs/select.sh:39-105` (`manifest_path`, `targets[].src_path`) and
  `sh/rs/c-tests.sh:36` (`.package_id | test("/moq-c#")`). Reproduce it,
  fix the filter, and make a jq error in selection fail the run instead of
  selecting less. It may relate to
  [moq-dev/moq#7289](https://github.com/moq-dev/moq/pull/7289), which
  moved the land base off jq on the worker.
- **send-depth on a pin change**: #7280 (merged) runs `just rs send-depth` on a
  pinned `legacyPackages.rust-nightly` in `flake.nix`, selected when moq-net
  or a dependency changes. A diff that only bumps that pin selects nothing,
  because `flake.nix` is outside the `rs` dispatch scope. Select send-depth
  when the `rust-nightly` pin changes, without making every `flake.nix`
  edit select the whole workspace.
- **uniffi typecheck**: `test/wasm/uniffi/tsconfig.json` includes
  `cases.ts`, `check.ts`, `driver.ts`, and `size.ts`, but only Nightly's
  `just test wasm-baseline --uniffi` (`.github/workflows/nightly.yml:170`)
  compiles them. [moq-dev/moq#7193](https://github.com/moq-dev/moq/pull/7193)
  changed `follow`'s arguments and broke that job unseen until #7304 fixed
  it; #6946 found the same gap for `cases.ts`. The files import bindings
  that ubrn generates, so the check needs that generation step; measure its
  cost on the gate before wiring it in. If it is too slow for the gate,
  bring the numbers to the maintainer before settling for a Nightly-only
  typecheck.

Public API: none. Wire: none.
