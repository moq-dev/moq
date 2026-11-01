# [XS] Cheap checks run through quest gate --quick

## Goal

moq's cheap checks, such as `check quest`, the formatters, and markdown,
wait at most a short bounded time for `quest gate` admission instead of
queuing behind agent builds.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from moq-dev/quest's host-gate cutover for moq.pro and rust2 and its fixer repairs).

- Condition: [moq-dev/quest#324](https://github.com/moq-dev/quest/pull/324)
  lands, adding `quest gate --quick` (at most 60 seconds of admission wait),
  and moq's flake pins a quest that includes it.
- Cheap checks waited up to an hour behind load
  ([#7250](https://github.com/moq-dev/moq/pull/7250),
  [#7257](https://github.com/moq-dev/moq/pull/7257)). Mark them in the
  dispatcher's module table, not by command name, and leave builds and
  test suites under normal admission.
