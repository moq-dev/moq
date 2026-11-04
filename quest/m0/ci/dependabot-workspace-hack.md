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

Options to evaluate, in this order:

1. **Exclude the file.** Dependabot's `exclude-paths` for the cargo
   ecosystem, pointing at `rs/workspace-hack/`. Risk: excluding a workspace
   member may break Dependabot's workspace resolution, so it updates nothing
   or fails. Test it on a fork or a scratch branch before relying on it.
2. **Ignore rules.** `ignore` entries for the hack's pins don't scale: the
   pins are every unified transitive dependency, and an ignore also stops the
   same crate's legitimate direct bumps.
3. **Regenerate in CI.** A workflow that runs `cargo hakari generate` on
   `dependabot/cargo/*` branches and pushes the result. It needs a write
   token on a pushed branch, which conflicts with
   [Read-only pull-request CI](/quest/m0/ci/read-only-secrets.md); bring it
   to the maintainer before building it.

Pick the first one that works, and record the test that proved it. If none
works without a write credential, report the options to the maintainer.

Public API: none. Wire: none.

## Related

- [Unify dependency features with cargo-hakari](/quest/m0/ci/hakari.md) - the workspace-hack this keeps generated
