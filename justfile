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
# The C and C++ packages over moq-ffi.
mod c
mod cpp
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
mod carla 'demo/carla'
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
# TRANSPORTS picks what chat and fanout dial. Example: `just bench origin/main ws`.
bench $BASE="" $TRANSPORTS="quic ws wss tcp tls":
    BENCH_TRANSPORTS="$TRANSPORTS" bench/run.sh "$BASE"

# Compare admitted suites across checkouts and worker counts; see bench/README.md.
[positional-arguments]
bench-local *args:
    python3 bench/local.py "$@"

# Measure audio group sizes across relay publisher/subscriber and fanout shapes.
bench-audio:
    bench/run.sh --audio

# Compare one multi-threaded Tokio runtime with independent Tokio/epoll and
# io_uring workers; WORKERS defaults to every logical CPU.
# Example: `just bench-runtime 5 16`.
bench-runtime $ROUNDS="3" $WORKERS="":
    BENCH_RUNTIME_ROUNDS="$ROUNDS" BENCH_RUNTIME_WORKERS="$WORKERS" bench/run.sh --runtime

# Sweep the relay's QUIC shard count against room and fanout connection counts;
# 0 shards is the shared runtime. Example: `just bench-shard "0 1 8" "64 512"`.
bench-shard $SHARDS="0 1 4 16" $CONNECTIONS="16 64 256":
    BENCH_SHARD_COUNTS="$SHARDS" BENCH_SHARD_CONNECTIONS="$CONNECTIONS" bench/run.sh --shard

# Profile the relay at each worker count: std mutex waits by stack (lock), CPU
# stacks (cpu), or jemalloc heap and churn (heap), over one transport (quic, ws, wss, tcp, or tls).
# Example: `just bench-profile "cpu heap" fanout-heavy 4`, or `just bench-profile cpu chat 4 tokio-workers "" ws`.
bench-profile $MODES $WORKLOAD="fanout" $WORKERS="1 4 16" $RUNTIME="tokio-workers" $BASE="" $TRANSPORT="quic":
    BENCH_PROFILE_MODES="$MODES" BENCH_PROFILE_WORKLOAD="$WORKLOAD" BENCH_PROFILE_WORKERS="$WORKERS" BENCH_PROFILE_RUNTIME="$RUNTIME" BENCH_BASE="$BASE" BENCH_PROFILE_TRANSPORT="$TRANSPORT" bench/run.sh --profile

# Real-browser transport, CMAF, and publish/watch costs, optionally against a base:
# `just bench-browser origin/main`. `--smoke` is the bounded correctness lane.
[positional-arguments]
bench-browser *args:
    bench/browser/run.sh "$@"

# Install repo-wide tooling. Per-language deps install on first check.
install:
    bun install
    cargo install --locked cargo-shear cargo-sort cargo-semver-checks release-plz

# Lints, compiles, and tests only the packages the branch changed plus
# everything depending on them; `check --all` is the unscoped suite. BASE
# defaults to the PR base, then a main/release upstream, then origin/main.
# Rust compiles once: its test build doubles
# as the clippy gate (see `rs check-test`).

# Lint, compile, and test what the branch changed since BASE, plus its dependents.
check $BASE="":
    sh/dispatch.sh check "$BASE"

# The landing worker runs this gate in one job. The test build also enforces
# default-feature clippy; the feature matrix and optimized bindings run
# nightly and on release PRs. Missing tools fail under CI_STRICT. The host
# land gate also sets CHECK_FAIL_FAST=1, so a failing batch stops at its first
# failed module.

# Run the landing worker's Linux gate over the branch diff against BASE.
ci $BASE="":
    CI_STRICT=1 NEXTEST_PROFILE=ci sh/dispatch.sh ci "$BASE"

# Run the queue's Linux gate over every package: `just gate [MODULE|-MODULE]...`
# runs only the named modules, or skips each `-` one (see CHECK_MODULES).
gate *modules:
    CHECK_MODULES={{ quote(modules) }} just ci --all

# Diff every published API against its last release: `just api-diff [--deny] [rust|js|ffi]...`.
# `--deny` fails on any break, as backports into `release` must.
[positional-arguments]
api-diff *args:
    sh/api-diff/run.sh "$@"

# Report what CI selects before installing the dev shell or build caches.
ci-select $BASE="":
    sh/dispatch.sh ci "$BASE" --query

# Report the native platform suites before installing their toolchains.
ci-select-platform $BASE="":
    sh/dispatch.sh ci-platform "$BASE" --query

# Auto-fix lint and formatting for what the branch changed since BASE.
fix $BASE="":
    sh/dispatch.sh fix "$BASE"

# Build .oss-scanner/ with Anthropic's OSS Scanner tools/check: `just oss-scanner [REF] [REPO]`. REF must be pushed.
[positional-arguments]
oss-scanner *args:
    sh/oss-scanner.sh "$@"

# Build the packages.
build:
    just js build
    just rs build
    just py build

# Delete this checkout's build artifacts and caches; `all` includes agent worktrees.
clean SCOPE="here":
    sh/clean.sh {{ SCOPE }}
