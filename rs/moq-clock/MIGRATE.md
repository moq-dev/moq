# Migrating moq-clock

How to update your code for each breaking change, newest first.

## Unreleased

### `anchor()` is `origin()` (#6510)

Rename `moq_clock::anchor()` to `moq_clock::origin()`, matching `@moq/clock`; it still returns the fixed `(Instant, Wall)` pair.

### moq-clock is a time library (#5465)

moq-clock 0.10 was the "CLOCK over QUIC" example binary.
0.11 is a library: logical instants, wall time, deadlines, and a deterministic simulator.
The clock example lives on as `cargo run -p moq-tokio --example clock`, which takes `--connect` instead of `--url`.
