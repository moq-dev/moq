# [M] Hold HLS resources to the playlist's origin

## Goal

HLS import fetches only resources with the playlist's scheme and authority,
including every redirect hop, so a playlist cannot steer the importer to
another origin.

## Plan

Security-sensitive follow-up from source review at main 1d43c096e0, also
relevant to release. The filesystem boundary is settled: `file:` resources
are read only for a `file:` entry playlist, and other schemes are refused
with `Error::ResourceRefused`. What remains, decided same-origin by the
maintainer on 2026-10-22:

- A reference is accepted only when the URL it finally resolves to has the
  playlist's scheme and authority, whatever form the reference took
  (absolute, network-path, or relative). Check once where references
  resolve so every resource kind shares the rule, and cover each form in
  the tests. Cross-origin CDN references are refused.
- Every redirect hop is held to the same rule before it is followed.
- A caller-supplied `Config::client` keeps its own redirect policy, which the
  importer cannot override per request. It has no in-repo consumer, so
  drop or narrow it on `main` (a `MIGRATE.md` entry if it changes); a
  release port that keeps the API documents that the caller's client owns
  redirect handling.

Report the caller-visible policy and API change and consider a release
backport after validation.
