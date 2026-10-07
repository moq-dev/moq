# [S] An epoch update re-keys a dynamic claim

## Goal

`Dynamic::update` (Rust) and `Dynamic.update` (JS) may change the route's
epoch, or add or clear it. The change re-keys the claim downstream: fronts,
relay caches, and subscriptions in flight under the old epoch end with the
same hard switch as any newer epoch, and re-requests resolve under the new
one. The origin's own served broadcasts and pending requests carry over,
since two epochs may carry the same content; a handler that wants fresh
content under the new epoch closes its old broadcast itself.

Non-goals: the epoch a claim-served answer carries
([#5010](https://github.com/moq-dev/moq/pull/5010)'s claim-epochs plan), and
same-epoch repricing, which keeps every handle.

## Plan

Found by the final-head audit of
[#4942](https://github.com/moq-dev/moq/pull/4942): an A to B update keeps
serving A's cached broadcast from the origin (`rs/moq-net/src/model/origin.rs`
`AnnounceProducer::update`, `js/net/src/origin.ts` `Dynamic.update`), while
both docs promise the served broadcasts end.

Decided 2026-10-07: carrying the served and pending state over is correct,
because an epoch is a cache identity, not a content guarantee. Rejected:
refusing an epoch change in `update` (forces drop and re-claim for no
benefit), and ending served broadcasts on an epoch change (more model code,
and the handler can already do it).

- Rewrite both docs to the contract above.
- Regress in Rust and JS: an update A to B, A to none, and none to B ends a
  downstream subscription and relay front under the old epoch, and a
  re-request is served under the new one. If downstream already does this, the
  quest is the docs and tests; otherwise fix the gap at its source.
- Keep the same-epoch repricing case in the same test, proving handles survive.
