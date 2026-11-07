# [S] The Nix CI cache never expires a path the newest cached commit needs

## Goal

The newest `main` commit with a successful Cache run always substitutes fully
from the CI bucket: no store path it references has passed the bucket's
30-day expiry (`infra/nix/lifecycle.json`, an `Age` rule on every object).

## Plan

Raised by [moq-dev/moq#8114](https://github.com/moq-dev/moq/pull/8114), which
pins Smoke to that commit. First confirm whether `sh/gh/cache-runtime.sh` (or
the copy it runs) skips uploading paths already in the bucket. If it does, a
path that many commits reuse keeps its first upload's age and expires while
newer commits still need it. Then fix it, for example by refreshing reused
paths on upload, or by expiring only paths no recent commit references.

## Related

- [Verify the Nix runtime cache](/quest/m0/ci/nix-cache-verify.md) - the scheduled Smoke check this protects
