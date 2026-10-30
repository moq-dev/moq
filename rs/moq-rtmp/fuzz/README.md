# Fuzzing moq-rtmp

Coverage-guided fuzzing of the AMF0 decoder behind RTMP commands and script data, the
first bytes a publisher sends after the handshake.

```bash
cargo install --locked cargo-fuzz
just rs fuzz amf0
```

Extra arguments pass through to libFuzzer, so `just rs fuzz amf0 -- -max_total_time=300`
bounds a run. The Nightly workflow runs it for five minutes, resumes from the previous
run's minimized corpus, and uploads failure inputs as workflow artifacts.

## How it fits together

The target body is not in `fuzz_targets/`; it is in `moq-rtmp`'s hidden `fuzz` module
(`src/fuzz.rs`), because the vendored `rml` module is private, and so `just check`
replays it on the pinned stable toolchain. The shim here also registers the counting
allocator the body measures with.

`just rs fuzz` regenerates `seeds/` from `moq_rtmp::fuzz::seeds()` before each run. The
first input byte picks the message type (AMF3 data, AMF3 command, AMF0 data, or AMF0
command); the rest is its body. See [moq-mux's README](../../moq-mux/fuzz/README.md) for
the corpus layout and calling `cargo fuzz` directly.

## What the target asserts

Beyond "does not panic": reading a message through the session's borrowed views and
walking every value it holds allocates nothing. The views keep the encoded bytes rather
than a node per value, so what a peer sends cannot grow the heap.

## When something fails

Fix the bug, then commit the input under `regressions/amf0/`, which
`fuzz::tests::regressions` replays as part of the normal test suite.
