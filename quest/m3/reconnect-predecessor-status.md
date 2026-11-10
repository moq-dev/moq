# [M] A connection reports Migrating while its predecessor still serves

## Goal

When a GOAWAY's replacement session connects and then drops while the old
session still drains, the handle reports Migrating and exposes the old
session, not Disconnected with no session. Disconnected means nothing serves.

## Plan

Found 2026-11-10 by the reconnect model ([moq-dev/moq#8447](https://github.com/moq-dev/moq/pull/8447)):
`rs/moq-net/verify/reconnect/OfflineWhileServing.cfg` breaks
`OfflineMeansNothingServes`. `closed()` in `rs/moq-net/src/reconnect.rs`
queues `Action::Offline`, whose doc says nothing serves anymore, while the
predecessor still carries live subscriptions until its handover ends; the
handle reports Disconnected, and `established()` and `session()` return
nothing, so code waiting on them stalls.
`residual_a_dropped_replacement_reports_offline_while_the_predecessor_serves`
in `reconnect/tests.rs` replays it.

Decided 2026-11-10 (maintainer, /quest-plan): ✅ fix the behaviour: report
Migrating, with `Shared`'s live session swapped to the predecessor until it
retires (rejected: rewording `Action::Offline` and `Lifecycle` to allow it).
Mirror the status in js/net's `Reload` if it shares the shape. Flip the
residual test, move the rule into `Handover.cfg`, delete
`OfflineWhileServing.cfg`, and update the `verify-reconnect` skill.

Caused by: [moq-dev/moq#7904](https://github.com/moq-dev/moq/pull/7904), which
made the connect policy a poll machine with this action queue.

Public API: none, unless the fix changes what `Lifecycle` exposes.
Wire: none.
