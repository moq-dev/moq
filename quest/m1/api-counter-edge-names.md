# [S] Cumulative open/closed counters name both edges

## Goal

Every open/closed counter pair in the stats surface reads as two cumulative
counters: `sessions_opened` / `sessions_closed`, never a bare `sessions` next
to `sessions_closed`, so a reader cannot mistake the open side for the live
gauge. A bare plural, when one exists, is the gauge (`open - closed`), the way
moqsink's `sessions-opened` / `sessions-closed` already reads after #3679.

## Plan

At main `ea47bc344` the bare-plural pattern is `moq_net::stats::Presence
{ sessions, sessions_closed }` and `Traffic { announced, announced_closed,
broadcasts, broadcasts_closed, subscriptions, subscriptions_closed }`
(`rs/moq-net/src/stats.rs`). Both are the wire shape of the moq-stats JSON
tracks, so the rename reaches `rs/moq-stats`, the relay (`rs/moq-relay`),
`demo/web/src/stats.ts`, and `doc/bin/relay/config.md`. `moq_native`'s
`ConnectionStatsReader::presence` and moqsink read `Presence` and follow.

Recommended: `sessions_opened`, `subscriptions_opened`, `broadcasts_opened`,
and `announced` becomes `announces_opened` / `announces_closed` so the noun
matches the other three; keep `active()` as the gauge accessor. Serde defaults
mean an old consumer reads zeros for the open side rather than failing, so
land the relay and the demo together and note the field rename in the
moq-stats changelog. Ask before choosing the `announced` spelling.

Public API: breaking on moq-net and moq-stats, so on dev. Wire: the JSON field
names on every stats track change. Run the relay stats tests and the demo
build.

## Related

- [Schema and library](/quest/m2/qos/stats/schema.md) - reworks the same
  tracks on dev; land this rename first or fold it into that change
