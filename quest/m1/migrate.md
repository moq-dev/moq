# [M] Per-package MIGRATE.md

## Goal

Every hand-written published package carries a `MIGRATE.md` that tells a
downstream user how to update their code, config, or invocation for each
breaking change, and AGENTS.md and CONTRIBUTING.md tell every breaking PR to
add its entry there. Entries for the breaks already on `main` since the last
release are in place before the breaking cut.

Non-goals: wire-format and rollout guidance for operators (stays in
`doc/setup/upgrade.md`), and CI enforcement on PRs into `main` (the
[API-diff check](/quest/m2/release/migrate-audit.md) on PRs into `release`
catches misses).

## Plan

Decided 2026-10-16 (maintainer interview):

- Audience is downstream users upgrading across a breaking release.
  `CHANGELOG.md` stays release-plz generated and untouched.
- Every hand-written published package gets a `MIGRATE.md` now, even if
  empty: the published Rust crates, the `@moq/*` packages with a release
  script, `rs/moq-ffi`, and the binding wrappers (`py/moq-rs`, `kt/moq`,
  `dart/moq`, `go/wrapper`, `swift`, `cpp/moq`, `cpp/obs`). Generated FFI
  layers (`py/moq-ffi`, `kt/moq-ffi`, `dart/moq_ffi`, `go/ffi`, `swift/ffi`)
  get none: they have no hand-written API.
- Layout: `## Unreleased` on top, newest first; the release cut renames it to
  the version transition (e.g. `## 0.3 -> 0.4`) and history stays. One
  before/after snippet per break.
- Each binding wrapper's file shows the change in its own language, matching
  the Cross-Package Sync table that already makes a `moq-ffi` change touch
  every wrapper.
- Binaries (`moq-relay`, `moq-cli`, `moq-gst`, `obs`, ...) use the same file;
  entries map an old flag, env var, or config key to its replacement with an
  example invocation.
- `MIGRATE.md` is the single source. Move every API entry out of
  `doc/setup/upgrade.md` into its package's file; the page keeps the
  cross-cutting wire and relay rollout guidance plus an index linking every
  `MIGRATE.md`. `doc/lib` pages link to their package's file instead of
  repeating it.
- No CI check on `main`. AGENTS.md (Public API section) and CONTRIBUTING.md
  (PRs, Impact) instruct a breaking PR to add its entry; the API-diff check
  on PRs into `release` is the backstop.
- Backfill from the 87 `!:` commits since the `release` merge base and the
  existing upgrade.md entries, not an API diff; the release-PR check covers
  unmarked breaks. As of 2026-10-16 the heaviest are moq-net/@moq/net, moq-mux,
  moq-ffi and every wrapper, moq-tokio, moq-relay/moq-cli,
  moq-video/moq-audio, and @moq/hang/@moq/publish/@moq/watch.
- Ranked first in m1 so breaking work landing after it carries its entry.

Repoint every active quest that sends breaking entries to upgrade.md, so
they land in package files instead: `quest/m1/ffi-shape/README.md`,
`quest/m1/ffi-shape/stream-producer-name.md`,
`quest/m1/json-window-timestamps.md`, `quest/m2/release/c-retire.md`, and the
release epic's note that the upgrade guide describes public API
(`quest/m2/release/README.md`). Grep `quest/` for `upgrade.md` again when
landing, since other quests may add references meanwhile.

## Related

- [API-diff check](/quest/m2/release/migrate-audit.md) - verifies these files against the API diff on the cut PR
