# [S] Merge main into dev

## Goal

`dev` contains `main` at or after
[#4506](https://github.com/moq-dev/moq/pull/4506) (`c2e7b5815`), so dev has
the `ts::stats` module the TS stats rename works on.

## Plan

Unlike the other outside conditions, this is work: a PR merging `main` into
`dev` and resolving its conflicts. As of 2026-09-30 `main` is 130 commits
ahead of `dev` and `dev` is 119 ahead of `main`.
