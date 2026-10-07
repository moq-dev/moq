# [L] A replaced broadcast restarts announce consumers, and subscriptions stay sticky

## Goal

When the source behind a path changes, every announce consumer sees an
explicit `Restart` event, in Rust (`AnnounceEvent::Restart`) and `@moq/net`.
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
- **Sticky subscriptions (reverses the 2026-10-06 hard switch).** A newer
  epoch no longer ends subscriptions in flight with `Unroutable`
  (`Pick::Superseded`, `front.rs` `supersede`). The front keeps serving its
  subscribers until its route goes. A relay keeps its upstream subscription
  on the old route while any downstream still holds it. Update the line
  README's decision, the epoch tests from #4942, and #4962's gateway tests
  that assert `Unroutable`.
- **No pinned joins.** `Consumer::request` joins an existing front only while
  the route it resolved through still wins. Today a request joins an
  epochless front even after the winner changed
  (`origin.rs` `Consumer::request`, `pick`), so a player that re-requests
  gets the dead broadcast again. JS already swaps per path.
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
  lite-07 changelog's "ends subscriptions to the older one".
- **Players.** `moq play` (#4970) and `@moq/watch` restart on `Restart`
  instead of treating every epochless `Update` as a restart, so a GOAWAY
  drain or re-price no longer restarts playback.
  `moqsrc` (planned in #4960) switches on `Restart` too.

Tests: a newer epoch and an epochless source change each deliver `Restart`
in both languages, including a reconnect with identical route metadata; a
re-price delivers `Update`; an old subscription keeps
receiving after a replacement until its route goes; a re-request after
`Restart` resolves the new route, over a relay on lite-06, lite-07, and
moq-transport. On lite-06 and moq-transport, coalescing depends on read
timing, so those tests accept `Restart` or `End` then `Start`. Run `just drafts check` and `just test interop --all`.

Docs: update `doc/concept/moq-lite.md` (publisher epochs) and
`doc/lib/{rs,js}` announce sections inline.

Public API: breaking, a new `AnnounceEvent::Restart` variant (Rust) and
`"restart"` kind (JS). Wire: a new lite-07 announce message; older versions
send END then START where they sent UPDATE for a source change.
