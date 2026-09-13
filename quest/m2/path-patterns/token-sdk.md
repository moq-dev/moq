# [M] Token SDKs

## Goal

Published Rust and TypeScript token APIs and CLIs mint v1 by default, while an
explicit legacy mode remains available for callers that still target v0
relays.

## Plan

The default and CLI interpretation switch is itself a published semantic break,
even after M1 has landed versioned types. Target a subsequent `dev` release
cycle that includes the M2 v1 relay reader, not `main` and not the imminent
M1 release. Preserve the current v0 default through that release; explicit v1
library operations are already functional through the M1 claims quest.

This order keeps readers ahead of default writers without holding the imminent
release for the full pattern-enforcement rollout. Mark the later breaking
release clearly in the PR and migration documentation.

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

- A subsequent breaking dev release cycle is open and includes the v1 relay reader; this default switch must not target main.

- [Relay auth](/quest/m2/path-patterns/relay-auth.md) - the in-tree reader accepts v1 before issuers default to it

- [Claims](/quest/m1/api-token-claims.md)
