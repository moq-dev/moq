# [XS] lite-05 copy fill checks idleness without wall time

## Goal

`copy fill: lite-05 fills below the live edge only what a reader's ranges
name` (`js/net/src/lite/copy_fill.test.ts`) passes reliably alone and in the
loaded suite. It proves no FETCH went out by waiting 200 ms of real time in
`idle()`, and fails about 1 in 20 on main.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete, a follow-up of
[moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670)): ✅ fix it as
stated, as a child of this epic under its rules. Drive the wait with the
`Manual` clock the file's other cases already install, or assert on the event
that settles the copy, rather than lengthening the wait.

Public API: none. Wire: none.
