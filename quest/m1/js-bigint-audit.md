# [S] @moq/net holds every unbounded u62 as bigint

## Goal

Every u62 wire value `@moq/net` reads that a peer can push past 2^53 is held
as `bigint`, as group IDs are since
[moq-dev/moq#7681](https://github.com/moq-dev/moq/pull/7681), so a large value
round-trips exactly or is refused instead of losing precision. Each one
moved has a round-trip test past 2^53.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- ✅ m1 (the maintainer asked for this audit)
- m2

- Audit the u62 values still held as `number` in `js/net` (lite and IETF
  coding): request IDs, track aliases, object and frame IDs, subgroups, and
  timestamps, among others.
- For each, decide whether a peer can send a value past 2^53. Move those to
  `bigint`; for the rest, refuse a value past `Number.MAX_SAFE_INTEGER` at
  decode and record why `number` is enough.
- Test each moved value with an encode/decode round trip past 2^53, and each
  refused one with a decode that fails.
- Rust already holds them as `u64`; keep names and semantics mirrored.

Public API: a breaking type change in `@moq/net` for each value moved to
`bigint`, noted in the package's MIGRATE. Wire: none.

## Related

- [Address groups by timestamp](/quest/m1/time-addressing/js.md) - starts become `bigint` there too
