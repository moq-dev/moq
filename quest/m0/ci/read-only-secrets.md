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

## Plan

Decided 2026-10-28 (maintainer, in kixelated/quest's
[trust audit](https://github.com/kixelated/quest/pull/264)): enrolled
repositories keep pull-request CI read-only rather than kix shielding CI
behind forks. moq-dev/moq is a kix pilot repository, so it should pass
kix's [read-only CI check](https://github.com/kixelated/quest/blob/main/quest/m0/kix/ci-read-only.md)
before contributors are invited.

Found on 2026-10-28 from the settings API (the default workflow token is
already `read`):

- **Repository-level secrets** reach every branch's workflows:
  `CARGO_REGISTRY_TOKEN`, `CLOUDFLARE_API_TOKEN`, `DOCKER_USERNAME`,
  `DOCKER_PASSWORD`, `MAVEN_CENTRAL_USERNAME`, `MAVEN_CENTRAL_PASSWORD`,
  `SIGNING_KEY`, `SIGNING_PASSWORD`, `WINGET_TOKEN`, `R2_ACCESS_KEY_ID`,
  `R2_SECRET_ACCESS_KEY`, `R2_ACCOUNT_ID`, `GCP_SERVICE_ACCOUNT_KEY`,
  `CACHIX_AUTH_TOKEN`, `CLAUDE_CODE_OAUTH_TOKEN`, `META_MODEL_API_KEY`,
  `APP_ID`, `APP_PRIVATE_KEY`, `MBX_R2_ACCESS_KEY_ID` and
  `MBX_R2_SECRET_ACCESS_KEY`. Move each write-capable one into an environment
  limited to the branches or tags whose workflows use it (`main`, `release`,
  or release tags). Keep at repository level only those that can't write.
- **Organization secrets** visible here: `APP_CLIENT_ID`, `APP_ID`,
  `APP_PRIVATE_KEY` and `DISCORD_WEBHOOK`. The App key can write, so remove
  this repository's access to the organization secret and supply the key only
  through a protected environment. Gating the jobs that mint its token is not
  enough: any branch can add a workflow that reads a visible secret.
- **`pub.dev` and `pypi`** have no secrets, presumably OIDC trusted
  publishing, and no deployment branch policy. So a branch's workflow can
  claim either environment and get an identity the registry may trust. Limit
  both to the release branch or tags.
- **`MBX_R2_*` on pull requests**: confirm that the repository-level pair is a
  read-only credential, not just mbx's read-only mode, since
  [Verify the R2 Rust cache](/quest/m0/ci/mbx-r2-verify.md) reports
  "effective read-only" on release pull requests. Writes belong to the
  `cache-write` pair, which is already limited to protected refs.
- **`can_approve_pull_request_reviews` is on**, so a workflow's token can
  approve a pull request. Turn it off unless a workflow needs it.
- Moving a secret needs its value, so the maintainer does that part. Agents
  can change workflows, environment policies and workflow permissions, but
  only with the maintainer's agreement.
- Verify by pushing a branch with a throwaway workflow that prints which
  secrets are non-empty (never their values), then delete the branch.

## Related

- [Land main through quest land](/quest/m0/ci/land.md) - its gate credential also belongs in a protected environment
