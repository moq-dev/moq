# [S] Dependabot leaves the workspace-hack to hakari

## Goal

A Dependabot cargo group PR arrives with `rs/workspace-hack/Cargo.toml`
matching `cargo hakari generate`, so nobody regenerates it by hand. Dependabot
no longer bumps the hack's transitive pins and pulls in duplicate majors.
Dependabot still updates every other cargo manifest and `Cargo.lock`. Done
when a Dependabot cargo group PR passes `cargo hakari generate --diff` (in
`just rs lint`) without a manual commit.

## Plan

Planned 2026-11-03 (maintainer, `/quest-plan`), from
[moq-dev/moq#7782](https://github.com/moq-dev/moq/pull/7782). Dependabot
edited the hakari-generated `rs/workspace-hack/Cargo.toml` by hand, bumping
transitive pins (rustc-hash 1→2, num-bigint, rand_chacha, spin, const-oid,
itertools, quick-xml, tower-http, windows-core). Those bumps only added
duplicate majors, and a maintainer commit had to regenerate the file back to
main's contents. Every cargo group PR does the same, since the cargo entry in
`.github/dependabot.yml` groups `"*"` from `/`.

Tested 2026-11-03 in [moq-dev/moq#7819](https://github.com/moq-dev/moq/pull/7819),
using a disposable archive of
[cd5a8476cb](https://github.com/moq-dev/moq/commit/cd5a8476cb21681ad08f5ac96eb433253f57547c)
and the official Cargo updater image at digest
`sha256:a49f8e5edbcbd179325a7cffbde6541cbc17b9ade1d86f61ff8d217b1eca5983`.
The Dependabot CLI verified its producer commit as
[6a43f5c18a](https://github.com/dependabot/dependabot-core/commit/6a43f5c18a7120386064c4afabb6a3e803d63d90).
The tests called the image's real Cargo fetcher, parser, update checker and
file updater without credentials; they did not create hosted update PRs.

- **Exclusion fails.** GitHub
  [documents `exclude-paths`](https://docs.github.com/en/code-security/reference/supply-chain-security/dependabot-options-reference#exclude-paths),
  but both `rs/workspace-hack/**` and the exact manifest path remove the
  required manifest from the resolver's temporary workspace. Fetching and
  parsing succeed; checking a root `rustc-hash` update then fails with
  `failed to load manifest for dependency workspace-hack` and a missing
  `rs/workspace-hack/Cargo.toml`. No update can be produced by that path.
- **The control reproduces the bug.** With no exclusion, Dependabot changes
  the hack's `rustc-hash` requirement from 1 to 2 and updates `Cargo.lock`,
  while the root requirement already uses 2.1.3. `cargo hakari generate --diff`
  passes on the original archive and fails on the updater's output, restoring
  the four generated requirements to 1.
- **Ignore rules have no file scope.** They match a dependency name, versions
  and update types, so excluding the hack's older line also excludes the same
  crate's legitimate direct updates. Do not maintain a generated list of pins.

No configuration-only fix was proven. Remaining work is to choose and test
an updater that regenerates the hack after dependency resolution, or wait for
Dependabot support. Hakari's maintainer
[uses Renovate for this reason](https://github.com/guppy-rs/guppy/issues/320#issuecomment-2408686014),
with a post-upgrade generation hook. Merely preserving an excluded manifest
as resolver input would still need testing when legitimate updates change
hakari's generated graph.

Do not build a workflow that writes back to `dependabot/cargo/*` before the
maintainer decides its scope and credential policy; it conflicts with
[Read-only pull-request CI](/quest/m0/ci/read-only-secrets.md).

Public API: none. Wire: none.

## Waiting

- Maintainer: choose the updater and write-credential boundary now that
  exclusion fails. Recommend evaluating a protected scheduled Cargo updater
  with hakari generation before it publishes its PR, such as Renovate, rather
  than granting a pushed branch a write token. Alternatively retain Dependabot
  and manual generation while seeking upstream support. Changing the updater
  or granting branch-writing CI a policy exception needs the maintainer's
  decision; no workaround or credential change has been implemented. Clear
  this wait by recording the choice and its credential policy here, then test
  a complete grouped update against the Goal.

## Related

- [Unify dependency features with cargo-hakari](/quest/m0/ci/hakari.md) - the workspace-hack this keeps generated
