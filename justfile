#!/usr/bin/env just --justfile
# Using Just: https://github.com/casey/just?tab=readme-ov-file#installation
#
# Every recipe is a menu entry: one line that runs a tool, or a script under
# sh/. Logic belongs in the script.

set unstable

# Per-language modules. Language-specific recipes live in their own justfiles.
mod js
mod rs
mod py
mod kt
mod swift
mod go
mod dart
# OBS Studio plugin (C++). See doc/bin/obs.md.
mod obs 'cpp/obs'
# Cross-language tests (`just test interop`, `just test drill`, ...).
mod test
# Demos and infra.
mod demo
mod infra
# IETF Internet-Drafts (`just drafts build`, `just drafts publish`).
mod drafts
# GitHub Actions workflow linting.
mod gh '.github'
# Shortcuts to avoid `demo::` prefix.
mod boy 'demo/boy'
mod pub 'demo/pub'
mod relay 'demo/relay'
mod sub 'demo/sub'
mod web 'demo/web'

# Run the demo by default.
default:
    just demo

# Alias for `just demo`.
dev:
    just demo

# Benchmark the current tree, or compare it with a commit: `just bench origin/main`.
bench $BASE="":
    bench/run.sh "$BASE"

# Compare one multi-threaded Tokio runtime with the same number of independent
# Tokio/epoll and io_uring workers; WORKERS defaults to every logical CPU.

# Compare a shared Tokio runtime with independent workers: `just bench-runtime 5 16`.
bench-runtime $ROUNDS="3" $WORKERS="":
    MOQ_BENCH_RUNTIME_ROUNDS="$ROUNDS" MOQ_BENCH_RUNTIME_WORKERS="$WORKERS" bench/run.sh --runtime

# Install repo-wide tooling. Per-language deps install on first check.
install:
    bun install
    cargo install --locked cargo-shear cargo-sort cargo-semver-checks release-plz

# Lints, compiles, and tests only the packages the branch changed plus
# everything depending on them; `check --all` is the unscoped suite. BASE
# defaults to the branch's upstream. Rust compiles once: its test build doubles
# as the clippy gate (see `rs check-test`).

# Lint, compile, and test what the branch changed since BASE, plus its dependents.
check $BASE="":
    sh/dispatch.sh check "$BASE"

# CI runs `check` as two parallel jobs, since one runner doing both takes about
# the sum of their times. Same dispatch, with MOQ_STRICT=1.

# Run half of `check` for CI: JOB `check` lints and compiles, `test` runs the tests.
ci $JOB $BASE="":
    sh/dispatch.sh "ci-$JOB" "$BASE"

# Auto-fix lint and formatting for what the branch changed since BASE.
fix $BASE="":
    sh/dispatch.sh fix "$BASE"

# Build the packages.
build:
    just js build
    just rs build
    just py build
    just js wasm

# Delete this checkout's build artifacts and caches; `all` includes agent worktrees.
clean SCOPE="here":
    sh/clean.sh {{ SCOPE }}
