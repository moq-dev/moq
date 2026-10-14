# [XS] Require the merge queue platform gates

## Goal

Platform failures block main merges and release PRs after the workflow changes
land. A maintainer activates the checks; no GitHub ruleset changes are made by
the implementation PR.

## Plan

- Main ruleset 2420853 already requires `Check` and `Test`. Keep both and add
  `Platform scope`, `macOS`, and `Windows`, from the GitHub Actions integration
  (15368). The workflow reports skipped contexts on main PRs, then executes
  the combined change in the merge queue. Add them right after the workflow
  lands, before enqueuing anything else; an older open PR reports them after
  its next push or edit.
- Release ruleset 24392632 has no merge queue. Keep its `Check` and `Test`
  contexts and add the same three platform contexts; release-targeting PRs
  execute them before merging. A release PR runs the workflows merged with
  `release`, so wait until the next cut carries this workflow there; earlier,
  backports would wait forever on contexts the old workflow never reports.
- Raise the main queue's check timeout from 60 to 90 minutes with the
  contexts: the full macOS suite took 43 minutes in one serial job, and the
  jobs allow 90.
- Require `Platform scope` too: a failed selector skips its OS dependents,
  and skipped jobs alone satisfy required checks.
- Confirm a platform-affecting main merge group runs at most one macOS job
  and one Windows job, while an unrelated diff skips both. Confirm a
  release-targeting PR runs the selected platform checks.
- Check and Test keep running on every PR and again in the merge queue; only
  the native jobs move off main PRs (maintainer, 2026-10-14). Linux-only
  platform jobs remain on PRs, and release packaging remains nightly.
- Nightly and releases share the five-job macOS cap. Measure overlap with
  queue runs before reserving capacity or reducing the queue build limit.

## Related

- [mbx on R2](/quest/m0/ci/mbx-r2.md) - gives platform jobs a Rust cache
