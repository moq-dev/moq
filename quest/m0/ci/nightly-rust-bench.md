# [S] Rust benchmark group fits its hour

## Goal

Nightly's `Rust benchmarks` workload (`just gh nightly rust`) finishes inside
its 60-minute job limit with room to grow, so every Rust benchmark in
`sh/gh/nightly.sh` runs each night.

## Plan

On 2026-10-16 (run 38897397428) it was cancelled at 60 minutes, partway
through `bench-gst-catalog`, so `moq-cli`'s stream-import benchmark never ran.
The hour split:

- 13.5 minutes entering the dev shell: `uniffi-bindgen-go`,
  `uniffi-bindgen-cpp` and `quest` built from source because the shell
  closure wasn't in the binary cache. The five other Nightly workload logs
  checked show the same 12-14 minutes.
- 46 minutes of benchmarks, 34 of them compiling: the group mixes
  `test`, `release` and `bench` profiles and several feature sets, so shared
  crates compile more than once. The largest steps were the multi-crate
  `cargo bench` (8 minutes compiling) and `bench-feed-fetch` (6 minutes).

Benchmarks are still being added to the group (#5663, #5675, #5698 since that
run), so caching the shell alone leaves it close to the limit.

Options, for the maintainer:

1. Split the group in two matrix legs along profile lines (release
   benches/tests vs the `test`-profile ignored tests), each with its own hour.
   Recommended: it bounds each leg and keeps the shared-build rationale within
   each leg.
2. Raise this leg's `timeout-minutes`. Cheapest, but the group keeps growing
   into whatever limit it gets.
3. Rely on [the Nix cache](/quest/m0/ci/nix-cache.md) to recover the 13.5
   minutes. Needed anyway, but not enough by itself.

## Related

- [nix.moq.dev replaces Cachix](/quest/m0/ci/nix-cache.md) - caches the dev shell every Nightly job builds
- [Nightly workloads pass](/quest/m0/ci/nightly-green.md) - the other Nightly failures from the same run
