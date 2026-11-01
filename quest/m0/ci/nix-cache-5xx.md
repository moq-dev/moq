# [S] A nix.moq.dev error does not fail a check

## Goal

A transient 5xx from `nix.moq.dev` costs a cache miss, not a failed
`just ci`: the dev shell still evaluates and builds what it could not
substitute.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from moq-dev/quest's host-gate cutover for moq.pro and rust2 and its fixer repairs).

- On 2026-10-31 one HTTP 500 for a `.narinfo` failed `check flake` and
  ejected a seven-PR batch in the host land gate.
- [The Worker fix](https://github.com/moq-dev/moq/pull/7537) turns R2 internal
  errors (10001) and service-unavailable errors (10043) into logged misses:
  the other bucket is still tried, and a resulting 404 is non-cacheable.
  Malformed input, bad bindings, authorization
  errors, and unknown failures still fail. No retries or client settings
  change. Both buckets and metadata/archive reads have local test coverage.
- A local HTTP fault-injection probe verified that the real dev shell
  evaluates and builds a fresh dependency from source with these 404s.
  Nix 2.35.2 also completed that probe against the original 500 response,
  while logging the error and disabling the cache. This does not reproduce
  the historical `check flake` failure. The available Cloudflare OAuth token
  received `Forbidden` when querying historical Worker logs, so the original
  incident's R2 code or edge origin remains unconfirmed.
- Decided 2026-11-01 (maintainer): land the narrow service-code handling
  without waiting for Workers Observability access to the original incident.
- Remaining: after the fix merges, the maintainer deploys the Worker with
  `just infra nix deploy`, then verifies cache misses and dev-shell
  evaluation/build through `nix.moq.dev`.
- The [Nix cache verification](/quest/m0/ci/nix-cache-verify.md) owns the
  release side of the cache.

## Related

- [Verify the Nix cache](/quest/m0/ci/nix-cache-verify.md) - releases and bucket sizes
