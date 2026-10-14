# [S] A stopped CLI exits while its stdin writer stays open on release

## Goal

The published-line `moq` CLI exits after signal-driven async cleanup even
when its stdin writer remains open and Tokio has a blocking stdin read in
flight. Backport the focused main fix without changing release's CLI flags,
stats interface, auth behavior, or wire format.

## Plan

Found while implementing [listening CLI sessions](/quest/m1/cli-serve.md):
a regression that keeps the stdin writer open independently of `Child::wait`
reproduced process teardown hanging after session drain. Tokio's default
runtime drop waits for that blocking stdin read. Read-only release inspection
found the same `#[tokio::main]` and blocking stdin path there.

Recommended unattended follow-up for the approved overnight batch:

- Land the main source fix first, then port only its explicit runtime
  ownership and shutdown after intended async cleanup. Preserve normal
  errors and exit status. Do not backport the new listening stats or drain
  interface as part of this compatibility fix.
- Reproduce on the chosen release base with a live stdin writer held outside
  the child wait. The regression must fail before the fix and pass after it,
  proving process exit follows cleanup without requiring stdin EOF.
- Preserve bounded shutdown and completed cleanup. Do not use
  `process::exit`, sleep, retries, or a larger timeout to hide the hang.
- Validate the clean committed head in Nix with strict local checks and the
  applicable CLI shutdown integrations; record tested SHA, release base,
  and results in a separate PR targeting `release`.

The existing failed-export load quest is related but not proven to share
this cause. Its cleanup closes stdin first, so this fix alone does not
complete that quest.

Public API: no signatures or CLI flags change; a stopped process no longer
waits for an unrelated stdin writer to close. Wire: none.

## Required

- [Listening CLI sessions](/quest/m1/cli-serve.md) - land the main runtime-teardown source fix before the compatible backport

## Related

- [Failed-export exit under load](/quest/m1/test-flakes-2/export-failure-exit.md) - independently identify the process and bound that fail under load
