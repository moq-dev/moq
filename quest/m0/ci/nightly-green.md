# [S] Nightly workloads pass

## Goal

Every Nightly workload that failed on 2026-10-16 (run 38897397428, commit
b4eab0db1) passes on the next scheduled run, except the Linux release builds
and `moq-gst` .deb/.rpm packages, which
[Release builds link again](/quest/m0/release-link.md) owns.

## Plan

Fixed in the `quest/m0/ci/nightly-green` PR, pending the next scheduled run:
Audit, Features (including a newer `moq-uring` break), JS benchmarks, the
browser backend baseline, the relay lock-wait profile (moved to x86), and
the Firefox and WebKit media players.

Still open:

- **moq-gst macOS formula**: `brew test` hangs in `gst-inspect-1.0`, and a
  probe formula with no moq plugin hangs the same way on `gst-inspect-1.0
  --version`. Neither the plugin nor plugin scanning is the cause. The tool
  hangs at startup inside Homebrew's test sandbox on the macOS 26 runner image
  (20261013) with GStreamer 1.28.8. The previous run had GStreamer 1.28.6 on
  image 20260907 and passed. The same binary runs instantly outside the
  sandbox. Choose between dropping the `gst-inspect-1.0` call from the formula
  test (the build step's unsandboxed smoke test already loads the plugin) and
  waiting for upstream.

Workloads added since that run (relay shard sweep, `rs send-depth`) have not
run on `main` yet; triage them from the next Nightly too.

## Related

- [First shared FFI release](/quest/m0/ci/nightly-ffi-reuse.md) - the check that found these
