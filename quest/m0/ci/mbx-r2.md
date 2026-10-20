# [M] mbx caches Rust builds in R2

## Goal

Every Rust CI job on Linux, macOS, and Windows restores compilation artifacts
from an R2 bucket instead of the Actions cache, so the 10 GB repository
budget no longer decides which jobs get a cache. Only workflows running on
`main` can write; pull requests and the merge queue read.

## Plan

- mbx supports `s3://` remotes with an R2 endpoint, needs no server, and
  already narrows itself to read-only outside protected-branch pushes. That
  client check is a convenience, not the boundary: the write key lives only
  in a GitHub Environment (e.g. `cache-write`) whose deployment branch policy
  allows `main`, so a same-repo PR cannot read it. A read-only key is a plain
  repo secret; fork PRs get neither and build cold.
- A private bucket, separate from the public Nix cache bucket. Namespaces per
  OS and architecture; expire old objects with an R2 lifecycle rule rather
  than a cleanup job.
- The pinned mbx (1.10.0) is well behind upstream; check which version the
  R2 backend and its read-only enforcement need.
- The bucket, lifecycle rule, and keys follow the apt/rpm bootstrap in
  `infra/README.md`: checked in where wrangler allows, commands documented
  where it does not. Creating them and the Environment are maintainer
  actions; ask before running each.
- Once R2 serves Linux, retire the Actions-cache path and the per-workflow
  Swatinem caches (Swift) rather than run both.
- Measure restore time against the 150-210s Actions restore of the ~6 GB
  store; R2 transfers only the objects a build misses, so it may be faster.

## Related

- [Release gates](/quest/m0/ci/release-gates.md) - the macOS and Windows jobs this gives a cache
