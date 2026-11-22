# [XS] Update smoke workflow to install moq instead of retired moq-cli

## Goal

`.github/workflows/smoke.yml` installs and verifies the published `moq` `.deb`
package rather than the frozen pre-rename `moq-cli` package, and
`test/smoke/freshness.sh` catches stale package names.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the exhaustive pre-cut
full-codebase audit): ✅ place this cut-blocking packaging bug in `m0` and gate
[the release cut](/quest/m3/release/cut.md) on it (rejected: putting it in `m1`
without gating the cut).

- [moq-dev/moq#3552](https://github.com/moq-dev/moq/pull/3552) (`f286fcf009`)
  renamed the published Debian CLI package from `moq-cli` to `moq`, but
  `.github/workflows/smoke.yml:190` and `:346` (plus the comment at `:196`)
  still install `moq-cli`. After `0.22.0` publishes, the smoke workflow would
  silently keep testing the old frozen `moq-cli` `.deb` instead of the new
  `moq` release.
- Update `.github/workflows/smoke.yml:190,196,346` to install `moq` (confirming
  whether any other smoke job or RPM/Homebrew lane still references `moq-cli`).
- Add a freshness guard in `test/smoke/freshness.sh` that verifies the
  installed package name and version match the released `moq` package.

Public API: none. Wire: none. Verify `smoke.yml` and `test/smoke/freshness.sh`
with shellcheck / workflow checks.
