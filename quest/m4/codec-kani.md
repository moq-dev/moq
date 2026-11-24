# [M] Kani proves the varint codecs and length arithmetic for every input

## Goal

Kani proves, for every input within a stated size and not a sampled few, that
moq-net's varint codecs never panic, never read past their input, and
round-trip every value, and that the length and bound arithmetic a peer's
numbers reach cannot overflow. The proofs run in Nightly on the same bodies
the fuzz targets run.

## Plan

Decided 2026-11-10 (maintainer, /quest-plan on what could be proved about
moq-net). Milestone m4: it hardens code whose design is not in question.

Kani is a bounded model checker for Rust (s2n-quic, Firecracker, and the
standard library use it). It runs the real code, so it cannot drift from it
the way a model can.

- Scope: both varint codecs in `rs/moq-net/src/coding/varint.rs` (every `u64`
  encodes and decodes back to itself at the length the draft gives; every
  byte string up to the longest encoding either needs more input or decodes to
  a value whose encoding is a prefix of it), the decoder's length-prefix and
  cap checks, and the 2^61 - 1 timestamp bound. Add other integer arithmetic
  a peer's numbers reach, such as stream credit and byte accounting, only
  where a proof finishes in minutes.
- Whole message decoders stay with the fuzzers: Kani scales badly through
  `Bytes`, `Vec`, and `HashMap`.
- ✅ Plain `#[kani::proof]` harnesses that call the existing `fuzz` module
  bodies with `kani::any()` bytes (rejected: the Bolero framework, a
  dependency whose one job here a ten-line harness does).
- Cost to the tree: one `cfg(kani)` check-cfg entry, and no shims around
  anything, unlike loom. Only Nightly compiles the harnesses, so its job is
  what keeps them building.
- A `just rs kani` recipe and a Nightly row, installing Kani as the fuzz job
  installs cargo-fuzz. Each harness states its unwind bound. A proof that
  stops finishing is narrowed in the open, never dropped or given a longer
  timeout to hide it.

Public API: none. Wire: none.
