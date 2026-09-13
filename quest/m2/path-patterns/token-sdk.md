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

- [Merge dev](/quest/m1/merge-dev.md) - the required M1 APIs must be available on main before this implementation starts

- [Relay auth](/quest/m2/path-patterns/relay-auth.md) - the in-tree reader accepts v1 before issuers default to it

- [Claims](/quest/m1/api-token-claims.md)
