# [XS] Verify the Nix runtime cache

## Goal

Hosted CI proves `main`'s `moq-relay` and `moq` runtime closures reach
`nix.moq.dev`: a successful Cache run uploads both, and a later Smoke nix run
substitutes them instead of compiling.

## Plan

[#5693](https://github.com/moq-dev/moq/pull/5693) wired `nix.moq.dev` into
CI. The maintainer ran the bootstrap in `infra/README.md` on 2026-10-19 and
2026-10-20. Check it with `curl https://nix.moq.dev/nix-cache-info`.

[#6588](https://github.com/moq-dev/moq/pull/6588) restored `cache.yml`:
every merge to `main` pushes the dev shell's build closure for x86 Linux, ARM
Linux, and ARM macOS to `nix-ci-moq-dev` (`sh/gh/cache.sh`). Hosted CI
entering the dev shell is verified: Nightly run 40506938673 (2026-10-27)
substituted it in about a minute with nothing compiled, against 12-14
minutes of source builds in the 2026-10-16 baseline.

[moq-dev/moq#7662](https://github.com/moq-dev/moq/pull/7662) adds a Cache
`runtime` job (`sh/gh/cache-runtime.sh`) that pushes the `moq-relay` and `moq`
runtime closures, after their Crane dependency artifacts, on x86 Linux,
Smoke's only current nix platform (runtime caching decided 2026-10-29).
[moq-dev/moq#7431](https://github.com/moq-dev/moq/pull/7431) dropped Cachix
from `main` (decided 2026-11-02), so Smoke reads `nix.moq.dev` without write
credentials.

Decided 2026-11-05 (maintainer, /quest-audit,
[moq-dev/moq#7975](https://github.com/moq-dev/moq/pull/7975)): ✅ keep only
this `main` evidence in m0 and move the release-tag proof, Cachix retirement,
and bucket sizes to
[Release tags substitute from nix.moq.dev](/quest/m2/release-nix-cache.md)
behind the release cut. Not chosen: moving the whole quest to m2; merging
`main` into `release` now.

Use the scheduled and push runs; never dispatch Nightly for this evidence.

## Waiting

- moq-dev/moq: Once
  [moq-dev/moq#7662](https://github.com/moq-dev/moq/pull/7662) merges, record the first successful Cache run's
  `Runtime (ubuntu-24.04)` job uploading both closures
  (`gh run list --workflow cache.yml --branch main`), then the next Smoke run
  on that head or later (the 08:00 UTC schedule) whose
  `ubuntu-latest / nix` job copies `moq-relay` and `moq` from
  `https://nix.moq.dev` instead of compiling them.

## Related

- [CI cost and latency](/quest/m0/ci/README.md) - the R2 and Environment decisions this verifies
