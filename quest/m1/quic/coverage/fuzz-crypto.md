# [S] Plaintext crypto for fuzzing

## Goal

A plaintext crypto session in `moq-quic`'s hidden `fuzz` module (no TLS
handshake, no packet protection) lets the `connection` fuzz target skip the
per-input TLS cost and decrypt the packets it injects, so fuzzing reaches the
frame layer at far higher throughput. It never ships in a non-test build.

## Plan

Decided 2026-10-16 with the maintainer:

- One implementation, in the hidden `fuzz` feature module. The Criterion
  benchmarks enable `--features fuzz`, as moq-net's already do, and replace
  their bench-local no-op protection (`NoProtection*` in
  `rs/moq-quic/benches/common/mod.rs`, ported from quinn `perf`'s
  `noprotection.rs`) with it.
- The `connection` target switches to it, so injected and mutated packets
  decrypt instead of being dropped at the AEAD. Today it runs about 64
  inputs/s, almost all spent in the handshake.
- Once packets decrypt, an injected or corrupted packet can carry valid
  STREAM or DATAGRAM frames, so the target's integrity checks (stream bytes
  match what was written, `check_datagram` matches what was sent) exclude
  the streams and datagrams those packets touch; no-panic still covers every
  input.
- Not a debug transport: nothing outside the `fuzz` feature can name it.
