# [L] A replaced broadcast restarts announce consumers, and subscriptions stay sticky

## Goal

When the source behind a path changes, an announce consumer sees an
explicit `Restart` event, in Rust (`AnnounceEvent::Restart`) and `@moq/net`:
always on lite-07, and on older versions and moq-transport when the END and
START arrive together (otherwise an `End` then a `Start`).
The source changes on a newer epoch, or, on a route without an epoch, when a
different route entry wins. A re-price or a same-source failover stays an
`Update`. Players start on `Start`, restart on `Restart`, and stop on `End`.

Subscriptions are sticky: a replacement no longer ends them. A subscription
stays on the route it resolved through until the application drops it or that
route goes, so the application decides whether to stay on the old broadcast
or follow the new one. New requests resolve the current winner and never join
a front pinned to a replaced route.

## Plan

Decided in planning (2026-10-07, from #4970's review):

- **Supersedes #5013's hard switch.** #5013 decided (2026-10-07) that a
  better route without an epoch takes over with a hard switch, ending
  subscriptions in flight. The maintainer chose sticky subscriptions plus
  `Restart` instead (2026-10-07), and this quest absorbs the rest of that
  takeover plan. Accepted consequence: a client that doesn't read announces
  (IETF subscribers, the go/python/c interop clients, third-party players)
  stays on a replaced broadcast until its route goes.
- **Source identity.** On a route without an epoch, a different route entry
  is a different source: any other announcing session, a peer reconnect
  included. Routes without an epoch can't resume across routes anyway, so
  the restart is honest. Rust's origin (`TableCursor::update`, `origin.rs`)
  compares `(hops, cost, source, epoch)`, servability, and captures: an
  epochless winner moving to another entry is an `Update`, or nothing at all
  when that metadata is identical (a reconnect under an identical route is
  deliberately invisible today). Change detection must key on the entry id,
  reversing that dedupe. A newer epoch already goes out as unannounce then
  announce there (the `prev.3 != meta.3` branch), which becomes the
  `Restart`. JS already ends and restarts on a new entry (`#runAnnounced`
  diffs entry identity). Both deliver `Restart` after this.
  Any new winner counts, including one decided by the rendezvous-hash
  tiebreak: without an epoch a relay can't tell a restarted publisher from
  an extra replica. So a cost change that moves the winner, or a change to
  an equal-cost pool (a transcoder worker joining, a relay peer
  reconnecting), restarts the paths it moves. Two routes with the same epoch
  and identical metadata are a seamless failover and stay invisible.
- **Equal-cost restarts keep the hash (2026-10-07).** The rendezvous-hash
  tiebreak stays ahead of recency: a transcode pool's replicas advertise
  the same path, and newest-wins would churn every viewer on each worker
  restart. `route_order` already breaks a full tie (same hop chain, so the
  same hash) by the newest announcement, so a restart on the old session's
  chain wins at once. Without an epoch, a restart on a different chain (say,
  through another relay) that loses the hash to its lingering old session
  wins, and sends `Restart`, only once that session closes (idle timeout)
  and its route is withdrawn.
- **Sticky subscriptions (reverses the 2026-10-06 hard switch).** A newer
  epoch no longer ends subscriptions in flight with `Unroutable`
  (`Pick::Superseded`, `front.rs` `supersede`). The front keeps serving its
  subscribers until its route goes. A relay keeps its upstream subscription
  on the old route while any downstream still holds it. JS `route()`
  (`js/net/src/origin.ts`) closes the old front when it swaps to a better
  entry, so it changes too. Update the epoch tests from #4942 and #4962's
  three gateway tests that assert `Unroutable` (`a_reconnect_replaces_the_stale_*`
  in `moq-rtmp` `server.rs`, `moq-srt` `ts.rs`, and `moq-rtc` `whip.rs`): the
  stale viewer keeps receiving, and a fresh request reaches the reconnect.
- **No pinned joins.** `Consumer::request` joins an existing front only while
  the route it resolved through still wins. Since #4942 an epochless front
  stays on its first route (`pick`'s `Some(None)` arm in `origin.rs`, the
  module docs in `model/front.rs`), so a request joins it even after the
  winner changed, and a player that re-requests gets the dead broadcast
  again. JS already swaps per path.
- **Wire.** lite-07 (`moq-lite-07-wip`) gains an explicit restart message on
  the announce stream. Older lite versions and moq-transport send
  `ANNOUNCE_END` then `ANNOUNCE_START` for a source change, a new rule for a
  relay's winner moving between entries (the draft only says so for a
  publisher replacing its own epoch), and a receiver delivers an end and
  start for the same prefix that are both pending before delivery as one
  `Restart` (no timer). `ANNOUNCE_UPDATE` stays a metadata update with no
  content claim. Draft edits in `drafts/draft-lcurley-moq-lite.md`: the new
  message; the routing rule that a per-subscriber winner change travels as
  ANNOUNCE_UPDATE (it does only without a source change); the SHOULD that a
  newer Epoch ends older subscriptions with UNROUTABLE (removed); and the
  lite-07 changelog's "ends subscriptions to the older one". The rule that a
  subscription between routes without an Epoch "stays on its route and ends
  with it" holds as written.
- **Players.** `moq play` (#4970) and `@moq/watch` restart on `Restart`
  instead of treating every epochless `Update` as a restart, so a re-price
  that keeps its winner, or a same-epoch failover, no longer restarts
  playback. An epochless drain onto a different entry still does.
  `moqsrc` (planned in #4960) switches on `Restart` too. On lite-06 and
  moq-transport, a pair not coalesced reaches players as `End` then
  `Start`: a stop, then a fresh play.
- **Rejected** (in #5013): `@moq/watch` resubscribing on `Internal` or
  `SessionClosed` (#4999), since players recover on announcements, not
  errors; announcing only once the old front dies, since viewers stay blank
  until the old session times out; switching only on a strictly better
  route, which leaves an equal-cost restart blank; the newest winning ahead
  of the hash, which lets a joining worker take every tied path; and waiting for lite-07
  to be the default.

Tests: a newer epoch and an epochless source change each deliver `Restart`
in both languages, including a reconnect with identical route metadata; a
re-price delivers `Update`; an old subscription keeps receiving after a
replacement until its route goes; a re-request after `Restart` resolves the
new route, over a relay on lite-06, lite-07, and moq-transport. On lite-06
and moq-transport, coalescing depends on read timing, so those tests accept
`Restart` or `End` then `Start`. Flip `better_route_keeps_the_incumbent`
(`rs/moq-net/tests/route_change.rs`: the better route wins new requests and
announces, while the incumbent's subscriptions continue) and
`identical_reannounce_is_invisible` (`origin.rs`) for routes without an
epoch. `route_dies_without_an_epoch` keeps its expectation, but its
`standby()` prices `B` strictly worse so it doesn't win before the trigger,
and a re-request then lands on `B`. Add a regression case where an
epochless replacement at equal cost, on a different hop chain chosen to lose
the hash, competes with the lingering old route: no `Restart` and new
requests stay on the old route until it is
withdrawn, then `Restart` and a re-request lands on the replacement. Run
`just drafts check` and
`just test interop --all`.

Docs: update `doc/concept/moq-lite.md` (publisher epochs) and
`doc/lib/{rs,js}` announce sections inline, plus `doc/bin/rtmp.md`,
`doc/bin/srt.md`, `doc/bin/rtc.md`, and `doc/bin/relay/cluster.md`, which
(after #4962) describe the hard switch: a reconnect "replaces it at once" and
stale subscriptions end with `Unroutable`.

Public API: breaking, a new `AnnounceEvent::Restart` variant (Rust) and
`"restart"` kind (JS). Wire: a new lite-07 announce message; older versions
send END then START where they sent UPDATE for a source change.

## Related

- [Apps](/quest/m0/broadcast-epoch/apps.md) - #4970 drives `@moq/watch` and `moq play` from announcements, which this relies on
- [Remove `--hop`](/quest/m0/broadcast-epoch/hop-removal.md) - drops the per-session hop stamp, so restarted routes often have identical metadata
