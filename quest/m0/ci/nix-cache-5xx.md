# [S] A nix.moq.dev error does not fail a check

## Goal

A transient 5xx from `nix.moq.dev` costs a cache miss, not a failed
`just ci`: the dev shell still evaluates and builds what it could not
substitute.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from moq-dev/quest's host-gate cutover for moq.pro and rust2 and its fixer repairs).

- On 2026-10-31 one HTTP 500 for a `.narinfo` failed `check flake` and
  ejected a seven-PR batch in the host land gate. Nix stops on a failed
  path-info lookup even with `fallback = true`, which covers only failed
  downloads.
- Prefer fixing it at the source: find what made the cache Worker return 500
  for a lookup and have it answer 404 (a miss) when its bucket read fails.
  If Nix still sees 5xx from the edge, retry the lookup in the Worker or
  through Nix's `download-attempts`.
- The [Nix cache verification](/quest/m0/ci/nix-cache-verify.md) owns the
  release side of the cache.

## Related

- [Verify the Nix cache](/quest/m0/ci/nix-cache-verify.md) - releases and bucket sizes
