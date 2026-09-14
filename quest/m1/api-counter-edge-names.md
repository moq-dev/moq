# [S] Cumulative counters name both edges as started and ended

## Goal

Every cumulative counter pair in the stats surface is `*_started` /
`*_ended`: `sessions_started` / `sessions_ended`, never a bare `sessions`
next to `sessions_closed`, so a reader cannot mistake the open side for the
live gauge, and "ended" covers a severed session where "closed" suggests a
graceful one. A bare plural, when one exists, is the gauge (`started -
ended`), the way moqsink's `sessions-started` / `sessions-ended` already
reads after #3679. A stats consumer built before the rename keeps reading a
relay built after it, and the other way round.

## Plan

At main `ea47bc344` the pairs are `moq_net::stats::Presence { sessions,
sessions_closed }` and `Traffic { announced, announced_closed, broadcasts,
broadcasts_closed, subscriptions, subscriptions_closed }`
(`rs/moq-net/src/stats.rs`). Both are the wire shape of the moq-stats JSON
tracks, so the rename reaches `rs/moq-stats`, the relay (`rs/moq-relay`),
`demo/web/src/stats.ts`, and `doc/bin/relay/config.md`. `moq_native`'s
`ConnectionStatsReader::presence` and moqsink read `Presence` and follow.

Rust fields become `sessions_started` / `sessions_ended`, `subscriptions_*`,
`broadcasts_*`, and `announces_started` / `announces_ended` (the noun matches
the other three; confirm the spelling before landing). Keep `active()` as the
gauge accessor.

Wire compatibility, both directions, without a version field:

- Deserialize tolerates both spellings with the canonical name winning, so a
  new consumer reads an old relay, a new relay, and the one release where a
  new relay emits both. A derived `#[serde(alias)]` is not enough: it rejects
  the both-spelling frame as a duplicate field, so the impl must accept a
  repeated key (custom `Deserialize` or equivalent) with defined precedence.
- Serialize emits both spellings for one moq-stats release, so an old
  consumer (which defaults a missing field to zero) still reads a new relay.
  A `Serialize` impl that writes the legacy names beside the new ones is the
  smallest way; the `.z` merge-patch twins carry the duplicates unchanged.
- Dropping the legacy names is a follow-up quest once the demo and any
  moq.pro consumer have moved, not part of this one.

Public API: breaking on moq-net and moq-stats (field renames), so on dev.
Wire: the stats tracks gain the new field names beside the old ones; nothing
is removed. Add a test that decodes a frame written with only the old names,
one written with only the new names, and one written with both spellings (the
actual serializer output) to the same `Presence`/`Traffic`,
and run the relay stats tests and the demo build.

## Required

- PR #3679 has merged - the `presence` surface and the moqsink names this
  quest renames against come from it

## Related

- [Schema and library](/quest/m2/qos/stats/schema.md) - reworks the same
  tracks on dev; land this rename first or fold it into that change
