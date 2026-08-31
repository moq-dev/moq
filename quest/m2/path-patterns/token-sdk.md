# [M] Token SDKs

## Goal

Published Rust and TypeScript token APIs and CLIs mint v1 by default, while an
explicit legacy mode remains available for callers that still target v0
relays.

## Plan

Land as breaking package releases.

- Update `moq-token`, `@moq/token`, both CLIs, examples, and generated help.
  Existing `--publish foo` now means exact `foo`; use `foo/**` for its subtree.
- Add an explicit version-0 or legacy option. Never infer a version from the
  presence of `*` or rewrite a bare v1 literal into a subtree.
- Make key generation accept immutable v1 scopes and print the version in
  inspect/debug output.
- Update every token issuer in the repository to v1, converting their intended
  prefixes to explicit trailing `/**`, and retain v0 verification tests.
- Mark the semantic break in changelogs and test Rust/JS/CLI interoperation.

## Required

- [Claims](/quest/m2/path-patterns/claims.md)
