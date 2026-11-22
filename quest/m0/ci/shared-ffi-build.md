# [L] Share moq-ffi builds and mbx cache across bindings

## Goal

Every language binding (`py`, `dart`, `kt`, `go`, `c`, `cpp`) reuses one
`moq-ffi` build per feature set (`openh264` and `testing,openh264`) under the
active `mbx` cache instead of compiling separate target directories or stripping
cache environment variables.

## Plan

Queue runs on the self-hosted runner show language binding checks rebuilding
`moq-ffi` and `openh264-sys2` repeatedly with high `mbx` cache misses (`check
dart` ~155s, `check py` ~126s, `check kt` ~102s, `check go` ~102s; `kt` alone
logged `mbx[cache]: 1 hits, 49 misses, 222.3 MiB stored locally`):

- `sh/dart/generate.sh` runs `env -i "${hook_env[@]}" cargo build-ffi --features
  testing --target-dir "$WORKSPACE_DIR/target/dart"`, which strips `MBX_*`,
  `CARGO_HOME`, `CC`, `CXX`, and `RUSTC_WRAPPER` and compiles into a separate
  `target/dart` directory.
- `py/justfile` compiles `moq-ffi --features testing,openh264` into `target/py`,
  recompiling `openh264-sys2` and workspace dependencies separately from
  `target/debug`.
- `kt`, `go`, `c`, and `cpp` each invoke `cargo build-ffi` or CMake's
  `moq_ffi_cargo` independently.

Steps:

- Unify `py` and `dart` onto one shared `testing,openh264` build directory or
  prebuilt library path so `openh264-sys2` and `moq-ffi` compile once per run.
- Preserve `mbx` and Cargo cache settings in `sh/dart/generate.sh` without
  breaking Dart `hooks_runner` freshness detection (for example by staging a
  prebuilt cdylib for the hook or isolating only the variables the hook tracks).
- Prebuild the default `moq-ffi` artifact once when multiple binding lanes are
  selected in `sh/dispatch.sh`, letting `kt`, `go`, `c`, and `cpp` consume it
  without redundant Cargo or C++ builds.
- Measure `queue.yml` binding wall time, runner CPU time, and `mbx` hit/miss
  counts before and after.

Decided 2026-11-21 (afk, `/quest-plan`):

- Placement: ✅ `quest/m0/ci/shared-ffi-build.md` (belongs in the active m0 CI
  cost and latency epic; rejected: top-level `m0`, `m3`).
- Scope: ✅ unify debug gate builds across bindings while keeping release
  packaging (`rs/moq-ffi/build.sh`) unchanged (rejected: dropping binding tests
  from the queue).
