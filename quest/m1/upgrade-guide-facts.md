# [XS] Upgrade notes match the code

## Goal

The upgrade guide and the CLI migration guide stop promising behavior the
code refuses: moq-transport drafts 18, 20, and 22 only, no capped grants, and a
`MOQ_EPOCH` that stops startup.

## Plan

Decided 2026-10-27 (maintainer), from an external release-to-main audit.
Each is a fact an earlier change made stale.

- `doc/setup/upgrade.md` says Rust and JavaScript speak drafts "18 through
  22"; `rs/moq-net/src/ietf/version.rs` has only `Draft18`, `Draft20`, and
  `Draft22`.
- `doc/setup/upgrade.md`'s "Capped grants need upgraded verifiers" bullet
  describes a `rate` cap that moq-auth now refuses as an object role
  (`rs/moq-auth/MIGRATE.md`). Replace it with the true rule: uncapped tokens
  keep their format, and an object role is refused.
- `rs/moq-cli/MIGRATE.md` says `MOQ_EPOCH` is ignored, but `check_env`
  (`rs/moq-tokio/src/cli/env.rs`) refuses every undeclared `MOQ_*`
  variable. Keep the refusal (fail loud) and say startup stops until the
  variable is removed.
- Grep the other `MIGRATE.md` files and `doc/` for removed `MOQ_*`
  variables described as ignored and fix them the same way.

Collapsing intermediate entries stays with the
[migration audit](/quest/m2/release/migrate-audit.md).

Public API: none. Wire: none.
