# [S] Keep pull-request CI read-only

## Goal

A workflow on any branch other than a protected one gets only read-only
credentials: a read-only `GITHUB_TOKEN`, no write-capable repository or
organization secrets, and no OIDC identity that a publisher trusts. Write
credentials live only in environments whose deployment policy allows
protected branches or tags. Agents and the kix.quest App push branches to
this repository itself, where GitHub hands a branch's workflows the
repository's secrets. So this is what keeps code nobody has reviewed away
from publishing and infrastructure credentials.

Done when a throwaway branch's workflow, printing only whether each secret is
non-empty, sees none of the write secrets listed below, then the branch is
deleted.

## Plan

Decided 2026-10-28 (maintainer, in kixelated/quest's
[trust audit](https://github.com/kixelated/quest/pull/264)): enrolled
repositories keep pull-request CI read-only rather than kix shielding CI
behind forks. moq-dev/moq is a kix pilot repository, so it should pass
kix's [read-only CI check](https://github.com/kixelated/quest/blob/main/quest/m0/kix/ci-read-only.md)
before contributors are invited.

Done in [moq-dev/moq#7166](https://github.com/moq-dev/moq/pull/7166): the
`publish` and `npm` Environments exist, `pypi` and `pub.dev` admit only
protected refs, the workflow token can no longer approve pull requests, and on
`main` every job that uses a write secret or a publisher's OIDC identity names
an Environment (see `infra/README.md`, "Write credentials"). The `release
tags` ruleset now also covers `moq-ffi-v*` and `moq-dart-v*`, and `release-js`
passes `JSR_TOKEN` from the `npm` Environment to `deno publish`, falling back to
JSR's repository OIDC trust while the token is unset. The repository's
`MBX_R2_*` pair is read-only: a branch's write to `mbx-moq-dev` is refused
with AccessDenied.

What remains, in order. Deleting a repository-level secret earlier breaks a
publish, since jobs that do not name `publish` still read it:

1. Backport the workflow changes to `release`. Release pushes and release
   tags run `release`'s own copies of the workflows, which differ widely from
   `main`'s.
2. The maintainer copies each value into the `publish` Environment:
   `CARGO_REGISTRY_TOKEN`, `CLOUDFLARE_API_TOKEN`, `DOCKER_USERNAME`,
   `DOCKER_PASSWORD`, `MAVEN_CENTRAL_USERNAME`, `MAVEN_CENTRAL_PASSWORD`,
   `SIGNING_KEY`, `SIGNING_PASSWORD`, `WINGET_TOKEN`, `R2_ACCESS_KEY_ID`,
   `R2_SECRET_ACCESS_KEY`, `R2_ACCOUNT_ID`, `CACHIX_AUTH_TOKEN`,
   `APP_PRIVATE_KEY` (the value jobs use today, the repository's, which
   shadows the organization's) and `DISCORD_WEBHOOK`.
3. Then deletes those repository secrets, the unused ones
   (`GCP_SERVICE_ACCOUNT_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`,
   `META_MODEL_API_KEY`, `APP_ID`), and this repository's access to the
   organization's `APP_PRIVATE_KEY` and `DISCORD_WEBHOOK`. `APP_CLIENT_ID`
   and `APP_ID` are identifiers and can stay. `MBX_R2_*` stays.
4. The maintainer names the Environment in each trusted publisher: npm's
   (`npm`, for every `@moq/*` package), PyPI's (`pypi`, for `moq-rs` and
   `moq-ffi`) and pub.dev's (`pub.dev`, for `moq` and `moq_ffi`). Until then a
   branch can still claim their OIDC identity by copying the workflow file.
5. JSR cannot require an Environment, so the maintainer creates a JSR publish
   token for the `@moq` scope, adds it as `JSR_TOKEN` in the `npm`
   Environment, then unlinks the GitHub repository from each JSR package
   (`@moq/auth`, `clock`, `hang`, `json`, `loc`, `msf`, `net`, `pattern`,
   `qmux`, `room`, `signals`, `video`) so a branch cannot publish through
   JSR's OIDC trust. Until the token exists, unlinking pauses JSR
   publishing.
6. Verify with the throwaway branch in the Goal, which also confirms each
   Environment refuses the branch.

## Related

- [Land main through quest land](/quest/m0/ci/land.md) - its gate credential also belongs in a protected environment
