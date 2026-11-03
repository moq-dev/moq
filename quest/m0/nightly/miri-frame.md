# [XS] Nightly Miri installs rust-src and covers the lock-free frame path

## Goal

Nightly's `Tests (rs miri)` job gets past cargo-miri's setup, and
`model::frame::tests` runs under Miri there, covering the lock-free unsafe
delivery path [moq-dev/moq#7508](https://github.com/moq-dev/moq/pull/7508)
added.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- ✅ m0, as a child of [Nightly passes on main](/quest/m0/nightly/README.md)
- m1
- m2

- The job's first run
  ([41483654426](https://github.com/moq-dev/moq/actions/runs/41483654426))
  failed before any test, as cargo-miri found no `rust-src`. Install it
  with the toolchain: `components: miri` in `.github/workflows/nightly.yml`
  becomes `miri, rust-src`.
- `sh/rs/miri.sh`'s strict pass already selects moq-net's `model::frame::`
  (line 85). Confirm #7508's tests are among them and pass under Miri
  locally, extend the selection if any are missed, and check the Nightly log
  shows them run.
- The rest of the job's time budget stays with
  [Miri nightly budget](/quest/m2/miri-nightly-budget.md) in m2.

Public API: none. Wire: none.
