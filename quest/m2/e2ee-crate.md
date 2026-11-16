# [M] End-to-end encryption is its own unpublished crate again

## Goal

The end-to-end encryption code lives in `rs/moq-e2ee` (crate `moq-e2ee`,
`publish = false`), not in `moq-crypto`, and the 0.22 cut publishes none of
it. `moq-crypto` is the crypto the transports share: the rind provider and
the CPU check, with no `e2ee` feature, and it no longer pulls `moq-net`,
`uuid`, or `base64` into any build for encryption. The docs stop saying a
hang broadcast can be encrypted, since no publisher, player, CLI, or binding
can make or play one. Nothing left in the tree mentions the deleted signing
module.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ E2EE moves back to its own `moq-e2ee` crate, because it is "not ready
  yet" (rejected: shipping it in moq-crypto 0.22 as a default feature under
  a new profile string, the pick made earlier the same day; leaving it and
  marking the module outside semver). This reverses the E2EE half of the
  2026-11-02 merge
  ([moq-dev/moq#7702](https://github.com/moq-dev/moq/pull/7702)), which
  joined two wrappers that shared a timestamp check: signing left in
  [moq-dev/moq#8855](https://github.com/moq-dev/moq/pull/8855), so the
  reason is gone.
- ✅ `moq-e2ee` is unpublished at the cut and until
  [the finished format](/quest/m3/e2ee-format.md) lands (rejected:
  publishing it as it stands, which would put an interim format out under
  `moq-e2ee-01` and force a third label on the finished one).
- ✅ The docs stop advertising encrypted hang.
- The format and API changes the audit decided are not lost: they are
  [the finished format](/quest/m3/e2ee-format.md)'s, after the cut.

Facts found by the audit:

- Nothing uses it: `grep -rl 'moq_crypto::e2ee' rs --include=*.rs` finds no
  file outside `rs/moq-crypto`, and no JS package imports `@moq/crypto`,
  which is `private` at 0.0.0 (`js/crypto/package.json:4,12`). No catalog
  field, CLI flag, or binding names it.
- It is a default feature (`rs/moq-crypto/Cargo.toml:21`) of the crate all
  ten transport and gateway crates depend on for `cpu` or `provider`, so
  each planned break to the wrappers
  ([moq-e2ee implements the moq-net traits](/quest/m5/net-wrap/e2ee.md))
  would be a breaking release of that crate.
- crates.io has `moq-e2ee` 0.0.25 (2026-11-11, from `release`), profile
  `moq-e2ee-00` with the old derivation: one-way physical names from
  `HKDF-Expand(prk, "moq-e2ee-00 name", 16)`. `main` uses the same label
  (`rs/moq-crypto/src/e2ee/limits.rs:2`) for AES-GCM reversible names and
  keys bound to the semantic name, so the two agree on a profile and a path
  and then find no track. `moq-crypto` has never been published (checked
  2026-11-12).
- The docs promise it: `doc/index.md:56` ("end-to-end encrypt your
  content"), `doc/concept/hang.md:13-17` and its Encryption section
  (`:227-241`), `doc/lib/rs/index.md:42`, and `doc/setup/repository.md:34`.
- Signing residue: the `signature` keyword (`rs/moq-crypto/Cargo.toml:12`),
  `rs/moq-crypto/MIGRATE.md:23-38` on `moq_crypto::sign` and `moq-sign`,
  which never shipped, and
  [the feature list](/quest/m3/release/feature-flags.md)'s `sign` feature.

For the PR:

- Move `rs/moq-crypto/src/e2ee`, its tests, `benches/protect.rs`, and the
  E2EE half of the README to `rs/moq-e2ee`, at a version above crates.io's
  0.0.25 so the later publish needs no jump. It depends on `moq-crypto` for
  `cpu` only, until
  [moq-dev/moq#8432](https://github.com/moq-dev/moq/pull/8432) removes that
  check. Update `rs/justfile` (`crypto-key` and the coroutine and doc
  lines that name `e2ee`), `.github/workflows/release-gate.yml`, the
  Nightly bench, `nix/Cargo.nix`, and `sh/api-diff` if it lists the crate.
- `moq-crypto` keeps `cpu`, `provider`, and `endpoint`; its description and
  keywords say so. `endpoint` and the mandatory `data-encoding` dependency
  are [the route switch](/quest/m2/multicast/switch.md)'s to delete with the
  Route Key, not this quest's.
- Recommended, not decided: `js/crypto` becomes `js/e2ee` (`@moq/e2ee`,
  still `private`), so the two halves share a name as AGENTS.md asks. It is
  free while the package is unpublished.
- Recommended, not decided: rename the profile on `main` to
  `moq-e2ee-01-wip` here (constant, salt, labels, `drafts/moq-e2ee-00.json`
  and `.ts`, the draft's Profile Version section), so nothing on `main`
  calls two derivations `moq-e2ee-00`, and an unpublished `-wip` label may
  keep changing. [The finished format](/quest/m3/e2ee-format.md) drops the
  suffix. The draft's changelog already heads the new derivation
  `draft-lcurley-moq-e2ee-01`.
- Docs: `doc/index.md` and `doc/concept/hang.md` describe only what ships,
  so the Encryption section and the opening paragraph's sentence go, and
  `doc/lib/rs/index.md` and `doc/setup/repository.md` list `moq-e2ee` as
  unpublished and experimental or drop the row. The draft's own page stays: a
  draft is not a shipped feature.
- Residue: drop the `signature` keyword, and delete the `MIGRATE.md`
  entries for names that never shipped (`moq_crypto::sign`, `moq-sign`, the
  `sign` and `e2ee` features);
  [the migrate audit](/quest/m3/release/migrate-audit.md) owns the rest of
  that file. `rs/moq-crypto/MIGRATE.md` then has nothing to
  say about `moq-e2ee` users, who have nowhere to move yet.
- `release` still holds `rs/moq-e2ee` at 0.0.25 and publishes it. After the
  cut it holds this crate with `publish = false`; check release-plz proposes
  nothing for it.
- `just check` and `just test interop --all` do not change behavior; the
  proof is that the workspace builds with every feature set `rs/justfile`
  checks and that `cargo tree -p moq-quic` shows no `uuid`.

Public API: `moq-crypto` loses its `e2ee` module and feature before its
first publish; `moq-e2ee` is unpublished, so nothing published breaks.
Wire: none (the profile label, if renamed, is unpublished).

## Related

- [The finished E2EE format](/quest/m3/e2ee-format.md) - the format and API changes, and the publish, that this defers
- [Retired package names](/quest/m3/release/retired-names.md) - `moq-e2ee` is no longer a retired name
