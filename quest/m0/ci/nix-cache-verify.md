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
and bucket sizes to an m2 quest behind the release cut. Not chosen: moving the
whole quest to m2; merging `main` into `release` now.

Superseded the same day (maintainer, /quest-unblock): the release-tag proof,
Cachix retirement, and bucket sizes moved to
[Release tags push to nix.moq.dev](/quest/m0/ci/cachix-retire.md), which
backports `release-nix.yml` to `release` now instead of waiting for the cut.

Use the scheduled and push runs; never dispatch Nightly for this evidence.

Checked 2026-11-06 (/quest-unblock): the upload half is done. The first successful Cache run after
#7662, [42119995773](https://github.com/moq-dev/moq/actions/runs/42119995773)
on 6b008527d3, pushed both closures (`moq-relay-0.17.2` and `moq-cli-0.14.2`,
the package behind the `moq` attribute) at 22:39:40Z. The next scheduled Smoke,
[42245927828](https://github.com/moq-dev/moq/actions/runs/42245927828)
(2026-11-06, started 15:45Z), fetched the deps from `nix.moq.dev` but still
compiled both binaries. Smoke builds `github:moq-dev/moq#...` unpinned, so it
built main as of 16:28Z (2916531c19), whose Cache run was cancelled, rather
than its own head. Any Smoke that starts while main is ahead of its last
completed Runtime upload misses the same way.

## Waiting

- moq-dev/moq: a scheduled Smoke run (08:00 UTC) whose `ubuntu-latest / nix`
  job copies `moq-relay` and `moq` from `https://nix.moq.dev` instead of
  compiling them. Check that the main commit it built had a completed Cache
  `Runtime (ubuntu-24.04)` upload; a miss on a commit without one says nothing
  about the cache.

## Related

- [CI cost and latency](/quest/m0/ci/README.md) - the R2 and Environment decisions this verifies
