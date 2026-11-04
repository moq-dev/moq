# [XS] Confirm scheduled open-GOP recovery passes

## Goal

The scheduled Interop run's "Harness regressions" step passes the
`open-gop.browser.ts` case "software video recovery trims orphan pictures
after join and later gap" on `main` with the typed fixture and direct
reporting of effect/page exceptions.

## Waiting

- moq-dev/moq: a scheduled Interop run on `main` including
  [moq-dev/moq#7814](https://github.com/moq-dev/moq/pull/7814) passes the
  open-GOP recovery case. Check with
  `gh run list --repo moq-dev/moq --workflow interop.yml --event schedule`,
  then inspect its "Harness regressions" step. Delete this quest once that
  case passes; the parent epic owns the whole-workflow pass.

## Plan

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from
[moq-dev/moq#7715](https://github.com/moq-dev/moq/pull/7715) and
[moq-dev/moq#7716](https://github.com/moq-dev/moq/pull/7716)): milestone m0,
a child of the Interop epic, since it breaks the scheduled run.

[moq-dev/moq#7672](https://github.com/moq-dev/moq/pull/7672) supplied the
missing broadcast name. [moq-dev/moq#7814](https://github.com/moq-dev/moq/pull/7814)
replaces the unchecked broadcast stub with a real local origin and a
manual-catalog `Broadcast`. It reuses the browser harness's existing page
and console error collection while polling recovery, so an effect exception
fails directly even when the recovery promise never settles. The recovery
assertions and 60-second deadline remain unchanged.

Only scheduled evidence remains. Do not dispatch Nightly manually or treat
a local pass as scheduled evidence. Triage a remaining failure at its cause.

Public API: none. Wire: none.
