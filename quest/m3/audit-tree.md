# [M] Audit the quest tree

## Goal

Run the `tree` audit: `quest audit tree` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

Baseline: 2026-11-23 at `890ea4e531` (before the 0.22.0 cut).

- The 2026-11-23 run consolidated `quest/m1/audit-{code,cost,deps,docs,tests,tree}.md` with the legacy `quest/m3/audit-{code,cost,docs,test-coverage}.md` files into these 6 `## Audit` stubs in `m3` behind the cut, keeping `audit-perf.md` and `audit-interop.md` as repo-specific audits.
- Check first: after the 0.22.0 cut shifts milestones (`quest/README.md`), verify that every moved quest's `Required` links, README Goals/Plan rules, and `m4` wire-cut recurrence updated cleanly. Also check the ~18 remaining historical backticked code-path references in `m3`/`m4`/`m5` quest prose (such as `rs/moq-studio/src/record.rs` and `rs/kixel/src/socket/stream.rs`, which name downstream repos rather than paths in this checkout).

## Audit

- tree

## Scheduled

- 2026-11-30 - one week after the 2026-11-23 pre-0.22 audit run

## Required

- [Release cut](/quest/m3/release/cut.md) - the release in flight ships before this run starts
