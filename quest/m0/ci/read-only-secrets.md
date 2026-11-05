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

Decided 2026-10-28 (maintainer, in moq-dev/quest's
[trust audit](https://github.com/moq-dev/quest/pull/264)): enrolled
repositories keep pull-request CI read-only rather than kix shielding CI
behind forks. moq-dev/moq is a kix pilot repository, so it should pass
kix's [read-only CI check](https://github.com/moq-dev/quest/blob/main/quest/m0/kix/ci-read-only.md)
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

Done in [moq-dev/moq#7202](https://github.com/moq-dev/moq/pull/7202), merged
into `release` 2026-10-31: the workflow backport, including its legacy Cachix
publishers and JSR token handling. For those publishers `publish` also admits
`moq-boy-v*`, `moq-gst-v*`, and `libmoq-v*` tags, and the `release tags`
ruleset also covers `libmoq-v*`. Verified 2026-11-03 during the
[landing recheck](https://github.com/moq-dev/moq/blob/17a81d6c64/quest/m0/ci/land.md):
neither `APP_CLIENT_ID` nor `APP_PRIVATE_KEY` remains at organization or
repository scope; `bot` and `publish` each hold both.

Done 2026-11-05 (maintainer, /quest-unblock): `publish` holds
`CARGO_REGISTRY_TOKEN`, `CLOUDFLARE_API_TOKEN`, `DOCKER_USERNAME`,
`DOCKER_PASSWORD`, `MAVEN_CENTRAL_USERNAME`, `MAVEN_CENTRAL_PASSWORD`,
`SIGNING_KEY`, `SIGNING_PASSWORD`, `WINGET_TOKEN`, `R2_ACCESS_KEY_ID`,
`R2_SECRET_ACCESS_KEY`, and `R2_ACCOUNT_ID` (check by name with
`gh secret list --repo moq-dev/moq --env publish`). Checked then:
`SIGNING_KEY`'s fingerprint ends `8FBE8982FDD3FCA2`, the key that signs
`dev.moq:moq` 0.5.3 and `moq-ffi` 0.4.22 on Maven Central; `WINGET_TOKEN` is
a fresh classic PAT owned by kixelated with only `public_repo`, which holds
the microsoft/winget-pkgs fork; the R2 pair lists both `apt-moq-dev` and
`rpm-moq-dev`. `SIGNING_PASSWORD` was not tested against the key. The
repository-level copies are still present.

Decided 2026-11-05 (maintainer, /quest-unblock): ✅ delete the repository
copies only after the next combined patch cut on `release` (release-plz PR
[moq-dev/moq#7936](https://github.com/moq-dev/moq/pull/7936)) publishes
through `publish` cleanly. Environment secrets shadow repository ones of the
same name, so that run proves the release jobs read the Environment's copies,
and proves `SIGNING_PASSWORD` if a Kotlin, apt, or rpm publish runs. Not
chosen: deleting them now.

`CACHIX_AUTH_TOKEN` is not in `publish` and stays out of the deletions for
now: `release`'s `cachix.yml` still reads it on `moq-relay-v*`,
`moq-cli-v*`, `moq-boy-v*`, `libmoq-v*`, and `moq-gst-v*` tags. That job
names `publish`, so it falls back to the repository copy. (`release`'s
`cache.yml` also reads it but fires only on pushes to `main`, so it never
runs there.) `main` no longer uses Cachix, and
[nix-cache-verify](/quest/m0/ci/nix-cache-verify.md) deletes the token once
a release cut merges `main` into `release`. Until then a branch can still
read it, so the Goal's check fails on it unless it moves into `publish`.

The maintainer's steps are under Waiting, in order. Deleting a
repository-level secret before a release proves the Environment copy breaks a
publish. Once they clear, an agent runs the throwaway-branch check in the
Goal, which also confirms each Environment refuses the branch, then deletes
the branch.

## Waiting

- Maintainer: decide where `CACHIX_AUTH_TOKEN` lives until Cachix retires.
  Recommended: copy it into `publish` now, beside the others, and delete the
  repository copy with them; `release`'s `cachix.yml` already names
  `publish`, and [publish-release-only](/quest/m0/ci/publish-release-only.md)
  already expects to remove it from there. Alternative: leave it at
  repository scope and hold the final check until
  [nix-cache-verify](/quest/m0/ci/nix-cache-verify.md) deletes it.
- Maintainer: once [moq-dev/moq#7936](https://github.com/moq-dev/moq/pull/7936)'s
  tags publish cleanly through `publish` (check every job in its tag runs
  with `gh run list --repo moq-dev/moq --branch <tag>`), delete the
  repository copies of the twelve secrets above, the unused
  `GCP_SERVICE_ACCOUNT_KEY`, `CLAUDE_CODE_OAUTH_TOKEN`, and
  `META_MODEL_API_KEY`, and this repository's access to the organization's
  `DISCORD_WEBHOOK` (already in `alert`). `MBX_R2_*` stays. Agents must not
  change credentials. Check: `gh secret list --repo moq-dev/moq` lists only
  `MBX_R2_*` (plus `CACHIX_AUTH_TOKEN` if it stays), and
  `gh api repos/moq-dev/moq/actions/organization-secrets` lists none.
- Maintainer: name the Environment in each trusted publisher, which needs
  the registries' account logins: npm's (`npm`, for the 17 public `@moq/*`
  packages), PyPI's (`pypi`, for `moq-rs` and `moq-ffi`), and pub.dev's
  (`pub.dev`, for `moq` and `moq_ffi`). Until then a branch can still claim
  their OIDC identity by copying the workflow file. Started in the browser
  2026-11-05 and paused at the npm login. Check each publisher's settings
  page shows its Environment.
- Maintainer: after the npm step, create a JSR publish token for the `@moq`
  scope and add it as `JSR_TOKEN` in the `npm` Environment, then unlink the
  GitHub repository from each JSR package (`@moq/auth`, `clock`, `hang`,
  `json`, `loc`, `msf`, `net`, `pattern`, `qmux`, `room`, `signals`,
  `video`), since JSR cannot require an Environment. Needs the JSR account;
  unlinking before the token exists pauses JSR publishing. Check:
  `gh secret list --repo moq-dev/moq --env npm` lists `JSR_TOKEN`, and each
  package's JSR settings show no linked repository.

## Related

- [Drop main from publish](/quest/m0/ci/publish-release-only.md) - narrows `publish` to `release` and release tags once `main` merges into `release`
- [Verify the Nix cache](/quest/m0/ci/nix-cache-verify.md) - retires Cachix and deletes `CACHIX_AUTH_TOKEN`
