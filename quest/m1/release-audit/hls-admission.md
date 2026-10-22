# [M] Review HLS resource admission boundaries

## Goal

HLS import preserves the caller's intended resource-access boundary across
secondary resource resolution and fetching.

## Plan

Security-sensitive follow-up from source review at main 1d43c096e0, also
relevant to release. Review resource admission in `rs/moq-hls/src/import.rs`
before the cut. Establish the affected configurations, address the boundary
at its owner, and add regression coverage for allowed and refused resources.
This public quest is deliberately a brief pointer at the maintainer's
request; it does not include sensitive reproduction details or claim a
demonstrated exploit. Report any caller-visible policy or API change and
consider a release backport after validation.

Planning settled from the 2026-10-22 security audit:

- A reference is accepted only when the URL it finally resolves to has the
  playlist's scheme and authority, whatever form the reference took
  (absolute, network-path, or relative). Check once where references
  resolve so every resource kind shares the rule, and cover each form in
  the tests.
- Every redirect hop is held to the same rule before it is followed.
- `file:` resources are read only when the playlist itself is a `file:` URL,
  which keeps local fixtures working.
- A caller-supplied `Config::client` keeps its own redirect policy, which the
  importer cannot override per request. It has no in-repo consumer, so
  propose dropping or narrowing it on `main` (a `MIGRATE.md` entry if it
  changes); a release port that keeps the API documents that the caller's
  client owns redirect handling.
