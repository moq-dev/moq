# [M] Audit the quest tree

## Goal

Run the `tree` audit: `quest audit tree` prints what it checks, what each finding becomes, and how to plan its next run.

## Plan

Baseline: 2026-11-26 (pre-0.22.0 audit, which deleted the `m9` lab milestone, added `quest/m2/api-fences.md`, moved non-breaking TS and kio quests out of `m2`, moved `aac-resume-stall.md` out of `m0/interop-green/`, promoted urgent consumer and lite-07 checks to `m0`/`m1`, demoted lower-priority/speculative quests, and pruned 14 obsolete/redundant quests).

- The 2026-11-23 run consolidated `quest/m1/audit-{code,cost,deps,docs,tests,tree}.md` with the legacy `quest/m3/audit-{code,cost,docs,test-coverage}.md` files into these 6 `## Audit` stubs in `m3` behind the cut, keeping `audit-perf.md` and `audit-interop.md` as repo-specific audits.
- Check first: after the 0.22.0 cut shifts milestones (`quest/README.md`), verify that every moved quest's `Required` links, README Goals/Plan rules, and `m4` wire-cut recurrence updated cleanly. Also check cross-repo code-path references in `m3`/`m4`/`m5` quest prose (such as moq-dev/studio's record.rs and kixel-ai/kixel.ai's socket/stream.rs) and verify whether open PRs (#9550, #9553, #9561, #9574/#9676, #9579, #9687) left any stale quest conflicts.

## Audit

- tree

## Scheduled

- 2026-11-30 - one week after the 2026-11-23 pre-0.22 audit run

## Required

- [Release cut](/quest/m3/release/cut.md) - the release in flight ships before this run starts
