# [S] A reused moq-transport alias delivers a group that beats its SUBSCRIBE_OK

## Goal

When a moq-transport publisher reuses a cancelled subscription's track alias
for a new subscription to the same track, a group that arrives before the
SUBSCRIBE_OK is delivered, not dropped as a straggler. A one-group catalog
fetched again from a publisher that answers every SUBSCRIBE with alias 0
(openmoq/moqxr) no longer hangs. Stragglers on other tracks' tombstones are
still dropped right away.

## Plan

Planned 2026-11-09 (maintainer, /quest-import of
[moq-dev/moq#8044](https://github.com/moq-dev/moq/issues/8044)). Milestone:
✅ m1 (rejected: m3, m4). The reporter offered to write the PR.

Today, `rs/moq-net/src/ietf/subscriber.rs` keeps a cancelled alias as
`Alias::Retired`, capped by `RETIRED_ALIAS_CAPACITY` and evicted only by
count. `ResolveAlias` drops a group on a retired alias at once, while an
unknown alias already waits up to `TRACK_ALIAS_TIMEOUT` for its
SUBSCRIBE_OK. Draft 22 §3.1.3 allows same-track alias reuse and names this
race.

Decided 2026-11-09 (maintainer): ✅ `Retired` records its track, and sending
a SUBSCRIBE for that track removes its tombstone, so an early group takes the
existing unknown-alias wait. Rejected:

- Clearing every tombstone on any SUBSCRIBE: it brings back the wait and the
  warn for other tracks' stragglers on busy sessions, which
  [moq-dev/moq#2993](https://github.com/moq-dev/moq/pull/2993) removed.
- The reporter's option 1 (a tombstone waits while its track's SUBSCRIBE is
  unanswered): it needs unanswered-request bookkeeping, and a missed
  decrement is a bug class [IETF request lease](/quest/m3/ietf-request-lease.md)
  already lists.
- The reporter's option 2 (every tombstone waits while any SUBSCRIBE is
  unanswered): it holds stragglers longer on a busy relay.

Caused by [moq-dev/moq#2993](https://github.com/moq-dev/moq/pull/2993), which
added the tombstones. Test: cancel a subscription, re-subscribe to the same
track, and have the publisher answer with the same alias while the first
group races ahead of the SUBSCRIBE_OK.
[moq-dev/moq#8402](https://github.com/moq-dev/moq/pull/8402) reshapes the
nearby SUBSCRIBE registration, so expect a small merge conflict there.

Public API: none. Wire: none.

## Closes

- [#8044](https://github.com/moq-dev/moq/issues/8044) - a group that beats its SUBSCRIBE_OK on a reused track alias is dropped

## Related

- [IETF request lease](/quest/m3/ietf-request-lease.md) - plans to split the alias table into its own module
