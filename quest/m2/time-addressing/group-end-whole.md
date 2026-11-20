# [M] lite-07's GROUP_END says only that a group is whole

## Goal

On lite-07-wip, GROUP_END drops its `End` field and stays a zero-length frame
meaning "this group is whole". A Group Stream that FINs without it carries a
closed, possibly partial group that failover may continue. A group's end
position lives only in hang's markers. moq-net ages a finished group with no
successor from its highest frame timestamp, and any other group from the next
group's start. Rust, `@moq/net`, and the moq-lite draft change together.

## Plan

Decided 2026-11-19 (maintainer, on closed
[moq-dev/moq#9192](https://github.com/moq-dev/moq/pull/9192) and this plan's
interview):

- ✅ Drop GROUP_END's timestamp; hang keeps its in-band end markers on every
  transport (rejected: readers infer stops from GROUP_END's position, as
  #9192 did; keep the position but ignore it). Reason: the timestamp has no
  moq-transport equivalent and saves only about a byte over hang's marker.
- ✅ Keep GROUP_END as a bare whole marker (rejected: drop GROUP_END
  entirely, with FIN meaning whole). Reason: a FIN cannot tell a partial
  group from a whole one. The draft lets a relay whose upstream closed
  mid-group deliver the frames it holds and FIN, and failover continues that
  group from another publisher; relay caching relies on knowing which copies
  are whole. moq-transport's END_OF_GROUP is the same bit, so `ietf` maps it
  one to one (`ietf/publisher.rs:7979` already sends END_OF_GROUP only for a
  whole group).
- ✅ Aging: a finished group with no successor ages from its highest frame
  timestamp; a group with a successor is bounded by the successor's start
  (rejected: age from when the last byte arrived; never age it). The rule
  ages up to one frame early, since frame durations are not on the wire. A
  pause makes the next start overstate a group's reach, which only keeps it
  a little longer; hang's markers give the player the exact end.

Where the timestamp lives today:

- Rust wire: `buffer_group_end` writes `varint(0)` then `varint(end)`
  (`rs/moq-net/src/lite/publisher.rs:6432-6450`; call sites `:3300`, `:3330`,
  `:6804`, `:6861`). Decode in `lite/subscriber.rs:1605-1609` and
  `:1791-1814`, applied at `:1543` through `recv.rs:111-131` (`finish_at`).
  The version gate `has_group_end` (`lite/version.rs:298-306`) also gates
  GROUP's Previous Start; keep that.
- JS wire: `frameDecoder` in `js/net/src/lite/group.ts:165` (type at
  `:122-125`, `readFrames` at `:189-215`), `groupEnd()` in
  `js/net/src/lite/publisher.ts:144-146` (sent at `:2488-2490`,
  `:2620-2621`), applied in `lite/subscriber.ts:2062`, `:2409`, `:3039`.
- API: `group::Producer::finish_at(end)` (`rs/moq-net/src/model/group.rs:2266`)
  and `group::Consumer::end()` (`:4255`); JS `Producer.finishAt` and the
  `Consumer.end` getter (`js/net/src/group.ts:729`, `:1028`, mirrors at
  `:469`, `:783`). `finish_at` becomes a timestamp-free whole finish, and
  `finish()` stays the closed one. Recommended name: `finish_whole()`, with
  `Outcome::Finished` carrying a whole flag instead of an end.
- Callers outside moq-net: `rs/moq-mux/src/container/producer.rs:809`
  (`close_group`), `container/consumer.rs:793-801` (feeds only
  `presented_end`), `container/ts/recovery_test.rs:497`, `:941`,
  `rs/moq-mux/tests/legacy_audio.rs:164`, `js/hang/src/container/legacy.ts:283-288`.
- Aging: Rust `rs/moq-net/src/model/track.rs` `reach` (`:2175-2179`),
  `own_end` (`:2203-2205`), `ended_at` (`:2210-2214`), used by `superseded`,
  `drifted`, `poll_drifted`, documented at `:2316-2321` and
  `model/group.rs:2257-2264`; tests at `track.rs:14674`, `:18642`. JS
  `js/net/src/track.ts:3243` (`#endedAt`), `:3252`, `:3280-3281`, `:3293-3295`.
- Draft `drafts/draft-lcurley-moq-lite.md`: group end states (183-187), the
  relay's closed delivery (225), Fetch (637, 2016), reach (829-830), expiry
  (842-843, 2000), Group Stream (871, 875-876), `Payload Length` 0 (2200),
  the GROUP_END message (2220-2237), and the changelog (2354). The TLA model
  mentions it in `rs/moq-net/verify/subscribe/SubscribeAccounting.tla`
  (lines 18, 44, 175, 252).

Tests: the GROUP_END codec round trip and fuzz (`subscriber.rs:4632`,
`:4651`, `publisher.rs:7285`, `fuzz/ingest.rs:388`) without the field; a
closed group still reads as partial and a whole one as whole across a relay;
the last group of a paused track ages out from its highest frame timestamp.
Run `just test interop --all`.

Public API: breaking in Rust and JS (`finish_at` and `end()` on groups).
Wire: lite-07-wip only, which is unpublished.

## Related

- [Start placement](/quest/m2/time-addressing/start-placement.md) - the by-start conversions the old consumers quest also held
