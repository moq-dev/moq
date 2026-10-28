# [S] Shipping size history admits the generated C package

## Goal

Nightly measures the generated C shipping archives while preserving comparable existing size history and refusing missing or renamed artifacts.

## Plan

[Shipping sizes](https://github.com/moq-dev/moq/actions/runs/40698329026/job/122881846466) collects successfully, then
`sh/size/report.py` rejects the changed artifact set. The newest usable
`history.json` is from 2026-10-24; the current report adds exactly four keys:
`release/moq-c-generated-<target>/moq-c-<target>.<archive>` for Linux ARM64,
Linux x86_64, macOS ARM64, and Windows x86_64. No existing key disappeared.

These archives come from `release-c.yml` and are shipping packages, not
unrelated diagnostics. Resolve their introduction explicitly. Prefer keeping
the existing series and starting the new measurements at their first sample;
retain the reporter's refusal of accidental removals or renames. Do not
silently discard the old history or exclude the new package to clear CI.

Public API: none expected. Wire: none expected.
