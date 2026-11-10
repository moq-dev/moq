# [S] Nightly's Windows platform job passes

## Goal

The `Windows` job of `platform.yml` passes on `main`, both as Nightly's
`platform / Windows` and on `workflow_dispatch` runs and release PRs. Every
Windows-only warning or Linux-only crate it trips on is fixed at its cause,
not silenced for the job.

## Plan

Planned 2026-11-09 (maintainer, from
[moq-dev/moq#8251](https://github.com/moq-dev/moq/pull/8251)'s follow-ups,
/quest-unblock). Known causes:

- `moq-vaapi` is not gated to Linux under `--all`, so the Windows build tries
  to compile it.
- `moq-relay`'s memory integration test leaves items unused on Windows,
  which `build.warnings = deny` turns into errors.
- The 2026-11-09 dispatch on `quest/m2/video-codec-receive`
  ([run](https://github.com/moq-dev/moq/actions/runs/42635646987)) failed
  `just rs test -p moq-video --no-default-features --features render` on a
  `dead_code` warning at `rs/moq-video/src/encode/backend/mod.rs:84`. Check
  whether `main` has it too.

Reproduce each on a `workflow_dispatch` of `platform.yml` for this branch
before and after the fix. Gate platform-specific crates and items with `cfg`
where they are defined, so a later Windows-only warning shows up as one new
error rather than a pile.

Public API: none. Wire: none.
