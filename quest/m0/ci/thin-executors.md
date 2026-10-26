# [S] Thin generic test executors

## Goal

`moq_clock::sim::run` and the executor behind `#[tokio::test]` are compiled once,
not once per test: each generic entry point only pins its future and hands a
`Pin<&mut dyn Future>` to a non-generic body. Record moq-net's test-target CPU
time and `cargo llvm-lines` total before and after.

## Plan

Decided with the maintainer on 2026-10-25, as the first rule of a "thin generics"
profile: a generic function delegates to a non-generic inner function, and
executors take `dyn Future`. Measured on moq main 7237beb17:

- moq-net's test target compiles 3x the CPU of its library (80 s vs 27 s).
- `moq_clock::sim::run::<F>` is instantiated 1,139 times, about 14% of the test
  target's 3.78M IR lines; with its closures and the thread-local helpers it
  pulls in, about 19%. Only its `poll(future)` call depends on `F`.
- moq has 1,473 `#[moq_clock::test]` and 2,650 `#[tokio::test]`.
  `#[tokio::test]` expands to tokio's `block_on::<F>`; measure it, and if it
  repeats the pattern, wrap it the same way (a moq test attribute or helper), or
  propose the change upstream.

kixelated/rust2's monomorphization budget lint enforces the rule once it exists.

Public API: `moq_clock::sim::run` keeps its signature.
