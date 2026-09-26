# [M] Bindings size profile

## Goal

Decide, with a benchmark, whether the moq-ffi builds for Swift, Android,
Dart, Go, and Python ship at `opt-level = "s"`. On aarch64-apple-darwin it
halved the stripped dylib from 20.0 MiB to 10.1 MiB (fat LTO, 1 CGU) and
shrank `__text` from 18.2 MB to 8.5 MB. The throughput cost is unmeasured.

## Plan

Decided in planning: adopt it only if the benchmark shows the cost is
negligible. The relay and CLI stay at opt-level 3 regardless.

Guidance:

- The `cc` crate builds the C dependencies at the profile's opt-level too:
  aws-lc, openh264, and libopus. Hold the codec and crypto crates at
  opt-level 3 with per-package overrides, and compare against building
  everything at "s".
- Measure what the bindings actually do: publish and subscribe throughput
  through moq-ffi, plus audio and video encode and decode. Wire the benchmark
  into CI, at least nightly.
- If it's adopted, use a named profile (for example `[profile.ffi]`,
  inheriting release) that every binding build path selects, so the relay
  and self-builders keep opt-level 3.

## Required

- [Release profile](/quest/m1/release-profile.md) - the baseline this compares against
