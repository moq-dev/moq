# [S] Nightly workloads pass

## Goal

Every Nightly workload that failed on 2026-10-16 (run 38897397428, commit
b4eab0db1) passes on the next scheduled run, except the Linux release builds,
which [Release builds link again](/quest/m0/release-link.md) owns.

## Plan

The first four still fail on `main` as of 2aa7a95d7, by source inspection
or a local `cargo deny`. The last two depend on the hosted runner and were
not reproduced locally.

- **Audit**: `cargo deny` fails on four rustls advisories
  (RUSTSEC-2026-0346 to 0349, fixed in rustls 0.23.46; `main` locks 0.23.45)
  and on the `kixelated/uniffi-rs` git source, which `deny.toml` doesn't
  allow. Bump rustls, and allow that one git source.
- **Features**: `moq-hls` tests call `build_inits`, which is
  `#[cfg(feature = "server")]`, so `--no-default-features --all-targets`
  fails to compile. Gate it the way its siblings are
  (`cfg_attr(not(feature = "server"), allow(dead_code))`) or gate the tests.
- **JS benchmarks**: `js/net/bench/lite-varint.ts` calls `Datagram.decode`
  without the `timed` callback it now takes.
- **Browser backend baseline**: `test/justfile`'s `wasm-baseline` defaults
  `MOQ_TEST_RUNS` to a path relative to `test/`, and `test/wasm/run.sh`
  changes into `test/wasm` before the relay writes its log there. Make the
  default absolute; the Nightly artifact path also expects `target/test`.
- **Relay lock-wait profile**: `perf record -e context-switches` fails on the
  hosted ARM runner ("PMU Hardware doesn't support sampling"). Decide whether
  the smoke test uses an event the runner supports or leaves hosted Nightly.
- **moq-gst macOS formula**: `brew test` times out in `gst-inspect-1.0 moq`
  after LaunchServices errors on the headless runner. Reproduce and find
  whether the plugin or plugin scanning hangs.

New workloads since that run (relay shard sweep, media engines, `rs send-depth`)
have not run yet; triage them from the next Nightly too.

## Related

- [Rust benchmark group fits its hour](/quest/m0/ci/nightly-rust-bench.md) - the cancelled Rust group
- [First shared FFI release](/quest/m0/ci/nightly-ffi-reuse.md) - the check that found these
