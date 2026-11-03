# [M] moq-crypto replaces moq-sign and moq-e2ee

## Goal

One crate, `moq-crypto`, and one JS package, `@moq/crypto`, hold MoQ's
content signing and end-to-end encryption, replacing `moq-sign`/`@moq/sign`
and `moq-e2ee`/`@moq/e2ee` before `main` is next cut into `release`. Both
halves share one timestamp check with moq-net, so they cannot drift from it.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run). The follow-up started from
[moq-dev/moq#7614](https://github.com/moq-dev/moq/pull/7614), which made
`moq_net::group::on_track` public; the maintainer asked to combine the two
packages instead of only deduplicating the check.

Decisions:

- ✅ Name it `moq-crypto` (Rust) and `@moq/crypto` (JS). Not chosen:
  `moq-secure`; keeping `moq-e2ee` and absorbing signing into it.
- ✅ Merge JS too, for parity. `@moq/sign` and `@moq/e2ee` are private and
  unpublished, so the rename costs nothing outside the repository. Not chosen:
  Rust only.
- ✅ Plain `sign` and `e2ee` modules, always built. Not chosen: Cargo features
  per half.
- ✅ `moq-e2ee` 0.0.19 is already on crates.io: at the cut, publish a final
  `moq-e2ee` whose README points at `moq-crypto`. No yank. Not chosen:
  leaving it as is; yanking it. Version levels stay with the maintainer.
- Both halves call `moq_net::group::on_track` instead of their own copies
  (for example `rs/moq-sign/src/group.rs:48`).

Keep the crate independent of hang and codec-specific publisher policy, as the
[E2EE epic](/quest/m2/e2ee/README.md) requires. Update every crate, package,
doc, binding, and quest that names the old packages, including the m2 E2EE
quests.

Public API: the crate and package names and their import paths change.
Wire: none.
