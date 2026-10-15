# [M] Swift, Dart, and C++ binding benchmarks

## Goal

Swift, Dart, and C++ run the [binding benchmark](/quest/m1/ffi-bench.md)
nightly with the same shape, metrics, and output as Python, Kotlin, and Go,
so every wrapper's FFI cost is tracked and shows on the trend site.

## Plan

Decided with the maintainer on 2026-10-15: every wrapper is covered because
each generator lowers bytes differently, and a generator regression only
shows in its own language. [Payload bytes](/quest/m1/ffi-bytes.md) needs
all six to prove its fix.

- Swift has no Linux toolchain in the dev shell; run it in the macOS job in
  `platform.yml`. Its numbers come from a different runner, so report its
  ratio against a Rust baseline measured in that same job.
- Follow whatever the harness quest settled on for shape and output; extend
  it rather than fork it.

Public API: none. Wire: none.

## Required

- [Binding benchmark harness](/quest/m1/ffi-bench.md) - the shape, baselines, and output these join
