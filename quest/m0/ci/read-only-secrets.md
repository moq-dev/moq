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

Decided 2026-11-05 (maintainer, /quest-unblock): ✅ retire Cachix instead of
moving `CACHIX_AUTH_TOKEN`. `release`'s `cachix.yml` still reads it on
`moq-relay-v*`, `moq-cli-v*`, `moq-boy-v*`, `libmoq-v*`, and `moq-gst-v*`
tags; that job names `publish`, which lacks it, so it reads the repository
copy. (`release`'s `cache.yml` also reads it but fires only on pushes to
`main`, so it never runs there.) The m0 quest [cachix-retire](/quest/m0/ci/cachix-retire.md),
from [moq-dev/moq#7982](https://github.com/moq-dev/moq/pull/7982), backports
`release-nix.yml` to `release`, drops `cachix.yml` there, and deletes
`CACHIX_AUTH_TOKEN` and the `kixelated` cache once a release tag proves
nix.moq.dev. So the deletions here leave `CACHIX_AUTH_TOKEN` to that quest,
and the throwaway-branch check waits for it. Not chosen: copying it into
`publish` now; leaving it at repository scope until the m2 cut.

Done 2026-11-06 (maintainer, /quest-unblock): trusted publishers name
their Environment. npm: `npm` is set and verified on `@moq/net`, `auth`,
`boy`, `hang`, `json`, `loc`, `msf`, `pattern`, `publish`, and `room`; npm's
Cloudflare blocked the automated save for `@moq/watch` and `@moq/signals`, so
those two remain (under Waiting). `@moq/qmux` is published by
moq-dev/web-transport, so it is out of scope here; `@moq/clock`, `libav`,
`ui`, and `video` are not published yet, and
[npm-first-publish](/quest/m3/release/npm-first-publish.md) claims them with
the `npm` Environment. PyPI: `moq-rs` already required `pypi`, and `moq-ffi`'s
any-environment publisher was replaced by one requiring `pypi` (verified on
its settings page). pub.dev: the maintainer set the required environment
`pub.dev` on `moq` and `moq_ffi` and reported both pages show it; agents
cannot view pub.dev admin pages.

Decided 2026-11-06 (maintainer, /quest-unblock): ✅ drop JSR rather than
create a `JSR_TOKEN`. The new m0 quest `jsr-retire`, planned in
[moq-dev/moq#8039](https://github.com/moq-dev/moq/pull/8039), owns removing
JSR publishing and its repository OIDC trust, so this quest no longer
creates a token or unlinks packages. Not chosen: keeping JSR behind a
`JSR_TOKEN` in the `npm` Environment.

Checked 2026-11-06: 3 of [moq-dev/moq#7936](https://github.com/moq-dev/moq/pull/7936)'s
14 tag runs failed on their first attempt. Cachix `moq-relay-v0.17.13`
([run 42129037703](https://github.com/moq-dev/moq/actions/runs/42129037703)):
the macOS and aarch64-linux jobs, both started 23:23:25Z, failed with
"Unable to fetch the information for the environment 'publish' targeted by
this job", while the ubuntu job in the same run, started 41 seconds later,
passed through `publish`. `publish`'s deployment policy admits
`moq-relay-v*` tags (with `main`, `release`, and the `libmoq`, `moq-boy`,
`moq-cli`, `moq-ffi`, and `moq-gst` tag patterns), so this was a transient
GitHub API failure, not a policy gap. Cachix `moq-gst-v0.4.23`
([run 42129724193](https://github.com/moq-dev/moq/actions/runs/42129724193))
hit a network fetch failure, and moq-gst `moq-gst-v0.4.23`
([run 42129723974](https://github.com/moq-dev/moq/actions/runs/42129723974))
timed out in `brew test` on the aarch64-apple-darwin tarball, which skipped
its Release job. None of the three points at the Environment's secrets.

The maintainer's steps are under Waiting, in order. Deleting a
repository-level secret before a release proves the Environment copy breaks a
publish. Once they and `cachix-retire` clear, an agent runs the
throwaway-branch check in the Goal, which also confirms each Environment
refuses the branch, then deletes the branch.

## Waiting

- Maintainer: re-run the failed jobs of the three
  [moq-dev/moq#7936](https://github.com/moq-dev/moq/pull/7936) tag runs
  ([42129037703](https://github.com/moq-dev/moq/actions/runs/42129037703),
  [42129724193](https://github.com/moq-dev/moq/actions/runs/42129724193),
  [42129723974](https://github.com/moq-dev/moq/actions/runs/42129723974));
  the Plan's 2026-11-06 check found each failure transient or unrelated to
  `publish`. Once every job passes, delete the repository copies of the
  twelve secrets above, the unused `GCP_SERVICE_ACCOUNT_KEY`,
  `CLAUDE_CODE_OAUTH_TOKEN`, and `META_MODEL_API_KEY`, and this repository's
  access to the organization's `DISCORD_WEBHOOK` (already in `alert`).
  `MBX_R2_*` stays. Agents must not change credentials or re-run workflows.
  Check: `gh run view <run> --repo moq-dev/moq` shows every job passed;
  `gh secret list --repo moq-dev/moq` lists only `MBX_R2_*` and
  `CACHIX_AUTH_TOKEN` (until `cachix-retire` deletes it); and
  `gh api repos/moq-dev/moq/actions/organization-secrets` lists none.
- Maintainer: on npm, set the trusted publisher's Environment for
  `@moq/watch` and `@moq/signals` (Settings, Trusted Publisher, Edit,
  Environment `npm`); Cloudflare blocked the automated save. Check: each
  package's Trusted Publisher settings show Environment `npm`.
- moq-dev/moq: [moq-dev/moq#8039](https://github.com/moq-dev/moq/pull/8039)
  adds `/quest/m0/ci/jsr-retire.md` to `main`. Then merge `main` here and
  link it under Related as the owner of JSR. Check:
  `gh pr view 8039 --repo moq-dev/moq --json state`.

## Required

- [Retire Cachix](/quest/m0/ci/cachix-retire.md) - deletes `CACHIX_AUTH_TOKEN`, which the throwaway-branch check needs gone

## Related

- [Drop main from publish](/quest/m0/ci/publish-release-only.md) - narrows `publish` to `release` and release tags once `main` merges into `release`
