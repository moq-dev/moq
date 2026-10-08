# [S] An epoch update re-keys a dynamic claim

## Goal

`Dynamic::update` (Rust) and `Dynamic.update` (JS) may change the route's
epoch, or add or clear it. The change is a source change downstream, under
the same rule as any other: announce consumers see a `Restart`, every
downstream subscriber (relays included) drops its copy of the old epoch and
resubscribes. Only an identical epoch splices, so no old-epoch group reaches
a new-epoch subscriber. The update invalidates this claim's old-epoch copies;
it does not override route selection. A re-request still resolves the
winning route, which ranks the newest epoch first and none last, so after A
to none, or A to an older B, another route still advertising A keeps
winning. The origin's own served broadcasts and pending requests carry over,
since two epochs may carry the same content; a handler that wants fresh
content under the new epoch closes its old broadcast itself.

Non-goals: the epoch a claim-served answer carries
([Claim-served epochs](/quest/m0/broadcast-epoch/claim-epochs.md)), and
same-epoch repricing, which keeps every handle and stays an `Update`.

## Plan

Found by the final-head audit of
[#4942](https://github.com/moq-dev/moq/pull/4942): an A to B update keeps
serving A's cached broadcast from the origin (`rs/moq-net/src/model/origin.rs`
`Dynamic::update` through `AnnounceProducer::update`), while its doc promises
the served broadcasts end. The JS `Dynamic.update` doc (`js/net/src/origin.ts`)
only says another epoch names another publisher instance.

Decided 2026-10-07: carrying the served and pending state over is correct,
because an epoch is a cache identity, not a content guarantee. Rejected:
refusing an epoch change in `update` (forces drop and re-claim for no
benefit), and ending served broadcasts on an epoch change (more model code,
and the handler can already do it).

Aligned 2026-10-08 with [Restart](/quest/m0/broadcast-epoch/restart.md), which
replaced the hard switch: downstream subscriptions are not ended by the
model; the `Restart` tells them to drop the old copy and resubscribe.

- Rewrite both docs to the contract above.
- Regress in Rust and JS: an update A to B, A to none, and none to B delivers
  `Restart` to an announce consumer and through a relay, the relay retires its
  old-epoch copy for new requests, and a re-request is served under the new
  epoch without any cached group from the old one. With a second route still
  at A, A to none leaves re-requests on that route, by normal route
  selection. If downstream already does this, the quest is the
  docs and tests; otherwise fix the gap at its source.
- Keep the same-epoch repricing case in the same test, proving handles survive.

## Required

- [Restart](/quest/m0/broadcast-epoch/restart.md) - the `Restart` event and the no-splice rule this update reuses
