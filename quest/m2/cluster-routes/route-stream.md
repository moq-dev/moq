# [XL] Each side says everything about routing on one stream

## Goal

In a lite-07 session each endpoint opens at most one unidirectional Announce
Stream and sends everything it says about routing on it, in order: the
prefixes it wants, and the routes, withdrawals, hold answers, claims, floors,
and SYNCED that answer the peer's prefixes. No routing message can overtake
another, so production is the route model's one FIFO per direction by
construction, whatever prefixes a side asked for. A change of token scope
edits the prefix set
instead of reopening a request. `announced()` in Rust and `routes()` in JS keep their APIs,
and lite-06 and older and moq-transport are untouched.

## Plan

Decided 2026-11-14 (maintainer, /quest-complete on
[moq-dev/moq#8768](https://github.com/moq-dev/moq/pull/8768), the m2 triage
before the cut): ✅ stays in m2, gating lite-07's freeze (rejected: moving the route stream and the
interest set to lite-08). Without it lite-07 would freeze with one router key per announce
stream, which the route model doesn't check, and keep the reopen-and-hand-over
machinery for its lifetime.

Decided 2026-11-10 (maintainer, /quest-plan on lite-07 routing).

Why: today a side's requests ride the streams it opened and its answers ride
the streams its peer opened, and QUIC orders no stream against another. The
first count to infinity the route model found is one message overtaking
another across that split, and the router keys each stream as its own
neighbour (`route/node.rs`), where the model and `Sim` have one neighbour per
session. This removes the ordering hazard as a class and makes a
token-scope change an edit.

Decided 2026-11-12 (maintainer, /quest-plan on the quests still written for
Babel): ✅ [the switch](https://github.com/moq-dev/moq/pull/8731) to the
path rule lands first, on today's streams, and this follows (rejected: this
first, which builds the echo and ROUTE_REQUEST on the new stream and then
deletes them; one wire change for both, which joins two XL quests). The path
rule ([epic](/quest/m2/cluster-routes/README.md) Plan item 27) has
no echo and no route request, and the switch puts the hold answer on the
publisher half, so a side's routing messages for one prefix already share a
stream. What is left for this quest is the order between a side's prefixes,
which still ride a stream each, one neighbour per session in the router,
and the edit.

Settled:

1. ✅ **One one-way stream per side** (rejected: one bidirectional Announce
   Stream per direction with the echo moved to the answering half, which
   still splits a side's messages over two streams, needs a rule for a peer
   that opened none, and makes the model grow a second queue per direction;
   one bidirectional stream per session opened by the client, which the
   client must open even when it wants nothing).
2. ✅ **Its interest is a set of prefixes edited in place**, with a floor per
   prefix, SYNCED per prefix, and swaps that keep what both sets cover:
   [the interest set](https://github.com/moq-dev/moq/pull/8728)
   settled and proved those rules, and this puts them on the wire.
3. ✅ **A token-scope change is an edit.** The subscriber's set is its local
   interests met with its scope's regions, one prefix per region, so a
   multi-region scope is certifiable. Plan items 16 and 17 of the
   [epic](/quest/m2/cluster-routes/README.md) (reopen and replace,
   and the old request's own budget) are superseded, and the overlapping
   reopened requests that could hold three times the announce cap
   ([moq-dev/moq#8375](https://github.com/moq-dev/moq/pull/8375)'s open
   review finding) cannot occur.
4. ✅ **A DEAD has a fixed size, and the routes it moves follow it as its own
   ROUTE_UPDATEs** (decided 2026-11-14 by the maintainer on
   [moq-dev/moq#8731](https://github.com/moq-dev/moq/pull/8731)'s decision
   22, [the comment](https://github.com/moq-dev/moq/pull/8731#issuecomment-6889336680)'s
   B; rejected: A, the parts with a More byte that the switch ships, where
   each part repeats the header and the sender splits by encoded size; C, a
   DEAD that holds every route through the hop with the moves after it as
   ordinary messages, which brings churn on every failure that moves routes
   and changes the model). A DEAD Closed is `Closed {Hop, Far, Gone, Moved
   Count}`, and the next Moved Count messages on the stream are ROUTE_UPDATEs,
   one per moved route. The receiver holds the DEAD until they arrive and
   then applies everything as one step, the route model's `Fail`, so
   `RouteHandoff.tla` and its configs are unchanged. A refused move is a
   refused ROUTE_UPDATE: the route ends, and its path still credits the DEAD.
   These are PROTOCOL_VIOLATIONs, so a DEAD always completes or ends the
   session:
   - any other message between the DEAD and its last update, whichever
     prefix it answers;
   - an update for a route that isn't live or doesn't cross the hop;
   - a count above the stream's routes through the hop.

   It replaces the parts in the same PR:
   - the DEAD codec (`rs/moq-net/src/lite/announce.rs`,
     `js/net/src/lite/announce.ts`): Moved Count in place of More and the
     moved list, and `Dead::split` goes;
   - `buffer_dead` (`rs/moq-net/src/lite/publisher/routed.rs`) writes the
     DEAD and then its updates, with nothing between them and no size split;
   - both receivers' reassembly (`Continued` in
     `rs/moq-net/src/lite/subscriber/routed.rs`, `continued` in
     `js/net/src/lite/subscriber.ts`) counts the updates instead of the
     parts;
   - the shared corpus (`rs/moq-net/fixtures/lite.json` and the fuzz seeds in
     `rs/moq-net/src/fuzz.rs`) drops the More cases and pins a DEAD with its
     updates and each violation;
   - the draft's DEAD section and Dead Hops describe the count.

   Test that a DEAD and its updates apply as one, in Rust and `js/net`, each
   failing when an update applies on its own, and that each violation
   closes the session.

Recommended wire, for the implementer to confirm or improve in the PR; each
departure is a choice to list there:

- A unidirectional stream type, Announce. An endpoint opens it when it first
  has something to say, opens at most one, and keeps it for the session. A
  second one, or one that ends before the session, closes the session. No
  reopening means no generation of streams to order against each other.
- The bidirectional Announce stream leaves lite-07.
- What a side sends as a subscriber: ANNOUNCE_REQUEST, now an edit (prefix
  ids to remove, then prefixes to add, each with its `Hidden`). What it
  sends as a publisher: one ANNOUNCE_OK per edit it applied, the marker where
  the edit takes effect, then ROUTE_START, UPDATE, HOLD, and END, the
  withdrawal that names a dead hop and the hold answer as
  [the switch](https://github.com/moq-dev/moq/pull/8731) shaped them, the
  ANNOUNCE messages, ANNOUNCE_FLOOR with a Prefix ID, and SYNCED with a
  Prefix ID. ANNOUNCE_START names the Prefix ID its suffix is relative to,
  and a compression base shares it.
- A side's Route IDs, Announce IDs, and Prefix IDs are one counter each per
  stream. A message names the peer's id where it answers the peer: a Prefix
  ID in ANNOUNCE_START, ANNOUNCE_FLOOR, and SYNCED.
- A removed prefix may still be named until the ANNOUNCE_OK for the edit that
  removed it, and never after. The subscriber holds a removed prefix's claims
  against its cap until then.
- The live set may be empty, never nests, and once encoded fits one
  ANNOUNCE_REQUEST (65,535 bytes), as SUBSCRIBE's Ranges do. Each claim is
  then presented once, plus one covering claim per route per prefix, so what
  a peer can make the other hold follows its own request, not a constant.
- A prefix outside the subscriber's grant lists nothing and is SYNCED at
  once; there is no per-prefix refusal. The local library refuses an
  `announced()` outside its known scope, as it does today.

Code this replaces, for lite-07 only:

- `SubscriberDriver`'s `Tasks<AnnouncePrefix>` and `Discovery`'s open and
  close of covering prefixes (`rs/moq-net/src/lite/subscriber.rs`,
  `rs/moq-net/src/discovery.rs`) become one stream machine whose edits follow
  `Discovery`. A narrower request no longer stays open under a broader one:
  one edit swaps them.
- `AnnouncePrefix::reopen`, `replacing`, `Routed::hand_over`, and
  `Slots::split` go, with their tests in `routed.rs` and
  `tests/integration/auth.rs`. Delete each outright if no other version uses
  it.
- `Routed` (`lite/subscriber/routed.rs`) and `Answer`
  (`lite/publisher/routed.rs`) become one per session, holding the prefixes.
  `Answer` already reads several node streams (one per mount).
- `Router::admit` (`rs/moq-net/src/route/path.rs`) files a scoped
  neighbour's route that is wider than the scope's regions at each region
  inside it, because a stream for a scope with several regions asks for a
  prefix above them all ([the switch](https://github.com/moq-dev/moq/pull/8731)
  added this). Two routes of one node at nested prefixes then both file at
  a region inside both, so one overwrites the other and an end of either
  drops the route there until the other sends again. With one prefix per
  region in the set, the peer answers at the region and nothing narrows, so
  the narrowing goes. Test a two-region scope behind a peer that holds a
  broad and a narrow route of one node, ending each in turn: the region
  inside both stays online through whichever route still lives, and goes
  offline only once both have ended. Decided 2026-11-14 (maintainer, on
  [moq-dev/moq#8731](https://github.com/moq-dev/moq/pull/8731)'s decision
  12): ✅ deferred here (rejected: tracking per filed slot which wire routes
  file into it in #8731, state this quest deletes).
- `route::Node::closing` and the quiet retire of a stream dropped outside
  it (`route/node.rs`), for lite-07: with each side's stream living as long
  as its session, every drop of one is a session close. Today a session's
  receive stream lands under the guard only because `Publisher::poll`
  returns the transport error before `poll_protocol` reaches the
  subscriber, which [#8731](https://github.com/moq-dev/moq/pull/8731)'s
  review flagged as resting on poll order. A guard taken on every poll
  would lock the node on each session wake, so the stream's lifetime is
  the fix.
- `route::Node` registers one neighbour per session for both directions,
  carrying [the interest set](https://github.com/moq-dev/moq/pull/8728)'s
  edits on it: `Out::Ask`, `Out::Edited` and `edited`, and SYNCED per
  prefix. Today `receive` and `answer` each take a fresh key
  (`route/node.rs`), so one session is two neighbours in the router, and
  each key's one edit is the prefix its stream opened with:
  `State::register` applies it at once and `flush` drops the router's asks
  and markers. A scoped peer's route wider than its regions is filed at each
  region inside it (`path::Router::admit`) rather than asked for per region.
- `js/net`: each `routes()` call opens its own stream today
  (`js/net/src/lite/subscriber.ts`), so JS gains the covering-prefix logic
  `Discovery` has. Its route layer keeps per-stream state, so this PR changes `js/net`
  too. A floor that changes what a claim covers, with no route change, also
  re-runs admission for the open requests it affects: `forwardAnnounced`
  (`js/net/src/connection/forward.ts`) asks `session.covers` only when a
  request is admitted, so a request can stay on a node that no longer
  covers it until something else refreshes it (flagged in
  [#8731](https://github.com/moq-dev/moq/pull/8731)'s review).

Also touching the same files, to merge around:
[moq-dev/moq#8458](https://github.com/moq-dev/moq/pull/8458) (its
per-announce-stream timer becomes per session) and
[moq-dev/moq#8457](https://github.com/moq-dev/moq/pull/8457) (warm routes on
the Announce Stream).

Tests: every ordering hazard the old shape had, on real sessions with mocked
time. A hold answer never overtakes a route, across prefixes too; a swap edit shows no claim offline and
re-sends nothing both sets cover; a narrowing edit drops only what it
uncovers; a scope renewal that changes the region keeps every still-covered
claim shown; a second Announce stream, a nested prefix, and a set over the
size limit each close the session; a peer that withholds ANNOUNCE_OK or a
hold answer holds no more than the announce cap. Replace
`tests/integration/lazy_discovery.rs`'s two-stream cases with edits.

Update the draft's Streams tables, Announce, Floors, Routing, the message
sections, and the lite-07 changelog in the same PR, and the `verify-route`
skill's note on `node.rs`'s per-stream keys. Run `just check`,
`just test interop --all`, and `just drafts check`.

Rollout: lite-07 is `-wip`, so a relay on this wire refuses one on the old
by misparse, not by version. Relays pinned to `main` roll together, as each
earlier lite-07 wire change did.

Public API: none expected. Wire: lite-07-wip only.

## Related

- [Announce interest streams share one route table](/quest/m4/announce-interest-memory.md) - lite-07 then holds one interest stream per side; the bound still matters for older versions
- [A regrant re-checks only the routes it can change](/quest/m4/regrant-route-recheck.md) - its per-stream product loses a factor on lite-07
