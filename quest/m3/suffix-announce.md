# [L] Suffix announce and interest in moq-lite

## Goal

A moq-lite subscriber can request announcements by suffix (`**/transcode.pro`)
and a publisher can advertise one, so a fleet-wide service claims every
matching path once instead of per prefix. moq-lite only: IETF sessions stay
prefix-only. Routing only: token claim patterns keep their suffix support
unchanged.

## Plan

Decided in the 2026-09-28 quest audit: suffix routing is dropped from every
other quest, which are prefix-only, and lives here as a moq-lite-only
extension. IETF MoQT refuses suffix matching, so a session negotiated as IETF
never carries it and there is no draft to converge with.

Today `AnnounceRequest` (`rs/moq-net/src/lite/announce.rs`) carries only a
`prefix`, and the origin's route table is a trie keyed by path segment
(`rs/moq-net/benches/origin.rs`). A suffix cannot walk that trie, so a naive
match costs the whole announce table on every announcement and every new
cursor. Requests hit the same wall: `request_broadcast` resolves through
`best_route`, which walks the prefix trie for the longest covering claim, so
a suffix advertisement must also be inserted where SUBSCRIBE and FETCH
resolution find it, with the same specificity rules as prefix claims.
Benchmark first: extend `rs/moq-net/benches/origin.rs` with suffix cursors
and suffix route lookups, each swept over publishers and subscribers. The
slopes decide between a reversed-segment index and abandoning the quest.

The wire field is version-gated like `hidden`. Update `js/net` and
`drafts/draft-lcurley-moq-lite.md` in the same PR. Token patterns
(`moq-pattern`, `moq_auth::Claims`) already match suffixes and do not change.

## Required

- A deployment needs a suffix claim that a service prefix (`.<service>/...`)
  cannot express

## Related

- [Wildcard](/quest/m0/wildcard/README.md) - prefix-only advertisements and the service-prefix layout this would extend
- [Path patterns](/quest/m1/path-patterns.md) - owns the pattern dialect a suffix interest would reuse
