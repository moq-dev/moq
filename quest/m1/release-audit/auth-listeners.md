# [S] Release AUTH terminal listeners during session churn

## Goal

A long-lived browser publishing session retains AUTH enforcement state in
proportion to its live work, not every past broadcast or grant change.

## Plan

P2, new since release with in-band AUTH
([moq-dev/moq#4039](https://github.com/moq-dev/moq/pull/4039)). At main 6369eca,
`js/net/src/auth_session.ts::enforceGrant` creates one session-lifetime
`closed` promise, then races it on every table/grant wake using native
`Promise.race`. The losing reactions remain attached until transport close.

Instrumenting `.then` on the real enforcer's shared terminal promise and
performing 1,000 broadcast-table changes increased reactions from 2 to 1,002.
The reproduction closed the transport and awaited enforcement afterwards.
This proves reaction retention; heap bytes were not measured.

Reuse the repository's listener-releasing race/scoped termination mechanism.
Check other repeated races over lifetime promises for the same ownership bug,
without rewriting unrelated single-shot races. Test churn with a stable small
live table, shutdown during pending authorization, and cancellation cleanup;
use controlled microtasks/time. Measure retention across updates and live
broadcast counts. No API or wire change is expected. The immediate consumer
is in-band AUTH enforcement on publishing browser sessions.
