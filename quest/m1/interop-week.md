# [XS] A week of nightly interop after #4529

## Goal

Seven nightly interop runs on `main` have completed since
[#4529](https://github.com/moq-dev/moq/pull/4529) added the player's `delay`
column to the interop trace.

This quest tracks a condition outside the repository. When it holds, delete
this quest and every `Required` entry that links it.

## Plan

#4529 merged on 2026-09-29 (`8b6abc0a1`). As of 2026-09-30 one scheduled
`interop.yml` run on `main` includes it, so the seventh lands around
2026-10-06. Check with `gh run list --workflow interop.yml --branch main`.
