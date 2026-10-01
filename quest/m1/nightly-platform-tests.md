# [S] Nightly tests on macOS and Windows

## Goal

A nightly job runs `cargo test` for `moq-auth`, `hang`, `moq-tokio`, and
`moq-native` on macOS and Windows runners and alerts like the other nightly
jobs, so platform-only behavior is tested and not just compiled.

## Plan

Today `.github/workflows/platform.yml` runs `just rs windows|macos`, which only
`cargo check`s (`sh/rs/select.sh`), and every `nightly.yml` job runs on
`ubuntu-24.04-arm`. [#4600](https://github.com/moq-dev/moq/pull/4600)'s
regressions exercise different clocks: `Instant` overflow in `moq-auth` fails on
macOS and `SystemTime` overflow in `hang` fails on Windows. On Linux they only
prove no panic, so CI never runs them where they matter.

Decision (2026-10-01): ✅ the platform-sensitive crates above. Rejected: the
whole workspace (slower, more flake surface) and only `moq-auth` + `hang`.

Add a `just` recipe per platform beside the existing check, wire it into
`nightly.yml`'s matrix, and make sure `alert.yml` covers it. Fix or quarantine
anything that only fails there at its cause.

Public API: none. Wire: none.
