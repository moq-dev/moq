# Fuzzing moq-uring

Coverage-guided fuzzing of the receive control-message walk, `RecvMeta::parse` in
`src/udp.rs`: the unsafe code that steps through the kernel's control buffer by the lengths
written in it and reads each payload as a C struct, once per received datagram.

```bash
cargo install --locked cargo-fuzz
just rs fuzz uring_cmsg
```

Extra arguments pass through to libFuzzer, so `just rs fuzz uring_cmsg -- -max_total_time=300`
bounds a run. The Nightly workflow runs it for five minutes, resumes from the previous run's
minimized corpus, and uploads failure inputs as workflow artifacts. It runs only on 64-bit
Linux, as the crate does.

## How it fits together

The target body is not in `fuzz_targets/`; it is in `moq-uring`'s `udp::fuzz` module
(`src/udp/fuzz.rs`), because the walk is private, and so `just check` replays it on the
pinned stable toolchain and `just rs miri` runs its seeds under Miri.

Inputs use moq-sock's format and seeds, `moq_sock::udp::fuzz::Control`, described in
[moq-sock's README](../../moq-sock/fuzz/README.md), so a buffer that splits the two walks
replays against both. See [moq-mux's README](../../moq-mux/fuzz/README.md) for the corpus
layout and calling `cargo fuzz` directly.

## What the target asserts

Beyond "does not panic": the walk reports exactly what a reference model reads from the
well-formed messages' bytes, with or without the malformed end after them, so a malformed
length stops the walk and keeps everything before it. The model reads each field at its
byte offset rather than through the C structs and applies messages in order; unlike
moq-sock, a recognized message without a valid value clears its field. The reported
stride is never zero.

## When something fails

libFuzzer writes the input to `artifacts/uring_cmsg/crash-<hash>` and prints the
`cargo fuzz` command to reproduce it. Fix the bug, then commit the input:

```bash
cp rs/moq-uring/fuzz/artifacts/uring_cmsg/crash-<hash> rs/moq-uring/fuzz/regressions/uring_cmsg/
```

`udp::fuzz::tests::regressions` replays everything under `regressions/<target>/` as part
of the normal test suite, so the case stays covered.
