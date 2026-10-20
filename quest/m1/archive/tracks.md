# [L] TRACKS

## Goal

A moq-lite-07 TRACKS request lists the tracks of one broadcast as its
producer creates them, each with its name and TRACK_INFO, marks with SYNCED
when the lister has drained what this hop held, and names
each track that no longer exists. The subscriber then SUBSCRIBEs to each track it wants, so
delivery stays pull: nothing is ever pushed into a pulled copy. A listing
never creates a track the publisher serves only on request (an any-name
`broadcast::Dynamic` handler such as a transcoder). A relay answers many
downstream TRACKS for a broadcast from one upstream TRACKS. Rust and JS both
speak it. A listed track's groups reach a late SUBSCRIBE from the cache;
its datagrams do not, since datagrams are lossy and never cached, so a
subscriber receives a track's datagrams only from when its SUBSCRIBE takes
effect.

## Plan

Decided by the maintainer on 2026-10-19, replacing SUBSCRIBE_ALL. Asked about
listing tracks against SUBSCRIBE_ALL: "ugh I guess the difference is push vs
pull. Maybe the right answer here is actually a method to list all of the
tracks within a broadcast/namespace. I'd rather do that for moq-lite than
deal with merging subscriptions and publishes." Choosing between a `.tracks`
track, a lite TRACKS message, and keeping SUBSCRIBE_ALL: "I think TRACKS,
which would actually be kind of nice as an IETF extension too? Oh actually we
don't need an extension; we could issue forward=0 to learn about all tracks
from IETF peers?"

Why the 2026-10-17 rejection of "a track-listing stream plus a SUBSCRIBE per
track" no longer holds:

- **The first-groups race is closed by the cache.** A SUBSCRIBE with no floor
  and a max age at least the publisher's starts at the oldest group the cache
  still holds, so a track's first groups reach a subscriber that learned of
  the track an RTT late, as long as the publisher retains them (Publisher Max
  Age). A group already past retention is a gap, which the archive records
  anyway. Datagrams are outside this: they are never cached
  (`rs/moq-net/src/model/track.rs`), so any late subscriber misses the ones
  sent before it, and SUBSCRIBE_ALL's pushed feed missed those sent before
  it attached too. A datagram track is listed like any other; its datagrams
  arrive from the SUBSCRIBE on, and its groups still come from the cache.
- **One mechanism cost more than two.**
  [moq-dev/moq#5931](https://github.com/moq-dev/moq/pull/5931) implemented
  SUBSCRIBE_ALL and found that a relay has to merge pushed feeds into pulled
  copies: copies shared by path and epoch, a feed attached to a track already
  requested, bounded SUBSCRIBEs for floors below the feed, aggregate
  preferences, and a choice between duplicate copies and splicing an
  epochless restart into an old copy. A listing plus ordinary SUBSCRIBEs
  needs none of that.

Scope is one broadcast, decided by the maintainer on 2026-10-19. ANNOUNCE already
discovers broadcasts under a prefix, with their epochs and restarts; a
prefix-scoped TRACKS would duplicate that and need every route handler to
answer for a whole prefix (#5931's `Lister`). A prefix consumer composes the
two: ANNOUNCE for broadcasts, TRACKS per announced broadcast. A route is
capability, not inventory: a covering-prefix announcement names no
broadcast, and a service serving paths under it on request lists nothing,
like an on-demand track. A prefix consumer therefore covers the broadcasts
announced at their own path (the lite draft already says an advertiser
SHOULD announce a concrete path once it is producing) and treats TRACKS
refused with NOT_FOUND on an announced path as "not a broadcast". Any other
refusal, UNAUTHORIZED included, keeps its reason and reaches the caller as
that broadcast's error, since a real broadcast refuses a lister without
permission. TRACKS names a
broadcast path and an Epoch and resolves like TRACK and SUBSCRIBE, so the
listing is pinned to the instance that answers it and ends with that
instance, never spliced into the next one. Rejected: a prefix-scoped TRACKS,
and a `.tracks` track (a track carrying a listing reuses delivery but mixes
control into the media namespace and needs its own retraction encoding).

Decided by the maintainer on 2026-10-19: TRACKS still gates the lite-07 cut
([Finalize moq-lite-07](/quest/m1/lite07-finalize.md)), so the archive line
has its discovery on the published wire.

Listing events, decided by the maintainer on 2026-10-19 ("for TRACKS, you
should report the TRACK_INFO as well for each track (maybe reuse the message
entirely?) as well as have an event for knowing when we're caught up, and
when a track no longer exists"):

- **Track added, with its TRACK_INFO.** Reuse the TRACK_INFO message
  encoding wholesale: the entry is the track name followed by an embedded,
  length-prefixed TRACK_INFO, so one decoder serves both streams and any
  field TRACK_INFO gains (refresh, compress) reaches listings without a second
  wire change. It fits because TRACK_INFO holds only fixed per-track
  properties and no name or stream-specific field; the name is the only thing
  wrapping it. A lister MAY skip the Track Stream for a track it learned this
  way, since the properties are the same immutable ones.
- **Synced.** An ordered SYNCED event, sent once, at the first of two
  points: the first time the reader's queue drains (its `poll_next` returns
  Pending), or once the reader has been sent N events, where N is the
  snapshot size it recorded when it subscribed. Decided by the maintainer on
  2026-10-19 (renamed from CAUGHT_UP the same day), with the bound added so
  live churn cannot postpone SYNCED ("It doesn't even need to be the exact
  count, it could be something like count * 2 as an upper bound"). N is
  enough, with no slack, because the reader's queue delivers in insertion
  order: the snapshot entries go in first, and collapsing only removes them
  or rewrites them in place, so nothing ever lands ahead of them. A removal may land before SYNCED, and an add
  and removal the reader has not reached collapse (see churn below), so the
  lister then holds every track this hop held that is still live. Entries
  after it are live. There is no
  count anywhere, decided by the maintainer on 2026-10-19 ("We don't need the
  count at all"): a count sent first commits to that many adds, which a track
  removed before a slow reader reaches its add would break. The event itself
  is the whole wire cost. The announce stream already carries the same
  event in lite-07, identical in shape.
  Synced means synced with this hop, decided by the maintainer on
  2026-10-19: a relay answers at once with what it currently holds and never
  waits on its upstream, since relays never block on peers. A cold relay may
  report zero; the upstream's tracks then arrive as live adds.
- **Track removed.** Names the track, sent when it leaves the broadcast's
  track table, the enumerable inventory. A `broadcast::Dynamic` handler may
  still serve that name on demand, so a later SUBSCRIBE may still succeed;
  removal promises only that the track is no longer listed. There are no entry IDs: a name is unique among a
  broadcast's live tracks, so the name identifies the entry, and a track
  created again under it is a later add of the same track: within one
  broadcast instance a name always means the same content, so its group
  sequences continue rather than restart. A publisher that restarts them is
  buggy and publishes a new broadcast (a new Epoch) instead, so an archive
  keyed by broadcast, track, and first group never collides on a re-add. Removing a name that is not live on
  the stream is a protocol violation. A removal costs the name's bytes instead
  of a varint, which is worth it: the receiver keys tracks by name anyway, and
  the sender needs no per-reader ID state (below).

Costs, accepted:

- **An RTT per new track** before its first group, on top of the listing's
  one-way delay. The cache absorbs it while groups are retained.
- **A subscription per track.** Each SUBSCRIBE takes a slot in the session's
  subscription cap (`session.subscriptions`, 1000 by default in
  `rs/moq-relay/src/limits.rs`; 10,000 for peers), so an archive of more than
  about a thousand tracks per session hits it. The cap does not change: it
  measures per-track serving state a relay holds either way, and
  SUBSCRIBE_ALL only hid that cost behind one slot per request. A large
  archive shards its prefix across sessions, or its operator raises the
  existing setting for that deployment. No new constant.

Guidance:

- Wire: a new lite-07-wip bidi stream carrying the three events above. The
  request names the broadcast path and Epoch. The stream ends when the
  broadcast instance does. Update
  `drafts/draft-lcurley-moq-lite.md` and its lite-07 changelog, then run
  `just drafts check` and `just test interop --all`. Older versions refuse the
  stream type; no emulation over SUBSCRIBE.
- Model: a broadcast published locally lists the tracks its producer holds.
  A broadcast served on request (`broadcast::Dynamic`, or a relay's copy of an
  upstream broadcast) answers through its handler; a session forwards the
  request upstream, and a handler that does not answer lists nothing. A relay
  never answers from its own track table, which holds only what was
  subscribed. The listing hangs off the broadcast copy a SUBSCRIBE already
  resolves, so it needs no new copy registry. #5931's
  `broadcast::Consumer::tracks()` is a starting point.
- Auth: TRACKS needs subscribe permission on the broadcast path, as TRACK
  does. Listing exposes names, not access; each SUBSCRIBE is authorized as
  usual.
- Untrusted input:
  - A hostile publisher can list without bound. Each listed track it makes a
    relay hold counts against that session's existing `announces` cap, the
    budget for discovery state a peer pushes at us; past it, the relay resets
    the listing rather than holding more. Names keep the existing length caps.
  - A hostile publisher can churn, adding and removing tracks as fast as it
    can send. Each reader keeps a pending queue keyed by name, as origin
    announcement consumers do (`OriginConsumerState` in
    `rs/moq-net/src/model/origin.rs`), decided by the maintainer on
    2026-10-19. It delivers in insertion order rather than origin's
    lexicographic `BTreeMap` order, which the SYNCED bound needs. A removal for an add still queued for that reader cancels
    both, before or after SYNCED. A removal for an add it already saw is
    queued once, and a later add of the name joins it, as origin's
    `UnannounceAnnounce` does. A reader's state is therefore bounded by the
    names it saw plus the live set (bounded by the `announces` cap), however
    long it stalls, with no global budget. Churn costs per-event work across
    a broadcast's listings (the same fan-out an Announce stream pays), never
    a backlog.
  - A hostile subscriber can open many TRACKS. Each takes a subscription slot,
    downstream listings of one broadcast share one upstream TRACKS, and a
    slow or stalled reader holds only its pending queue, bounded as above,
    not a copy of the listing's history.
- Fan-out: benchmark downstream listings against tracks per broadcast and
  broadcasts per relay, per the AGENTS.md fan-out rule; a new track's cost
  must not grow with the broadcast count.
- Split the JS half into its own quest if the PR grows too large to review.

Test on mock time, directly and through a relay: tracks present at the
request arriving before SYNCED, SYNCED sent the first time the reader's
queue drains, a cold relay answering SYNCED at once with what it holds and
the upstream's tracks following as
live adds, a track added later (a
SUBSCRIBE from the listing gets its first group from the cache, and its
datagrams only from the SUBSCRIBE on), a track
removed and created again under its name (removal sent before the new add),
add and remove churn collapsing for a stalled reader (including an initial
track removed before the reader reaches it, with SYNCED still arriving), a
stalled reader under continuous churn of new names getting SYNCED once it
has been sent N events, a
removal of a track the reader saw landing before SYNCED, a stalled reader
holding no more than the names it saw plus the live set under churn, a removal
naming a track that is not live refused, a TRACKS refused on a
covering-prefix route, the broadcast ending the listing, a new epoch at the same path not continuing an old
listing, a dynamic-only track never created, two downstream listings sharing
one upstream TRACKS, and the `announces` cap resetting an oversized listing.

Public API: a track-listing hook on `broadcast::Consumer` in Rust and
`@moq/net`, mirrored names, and a way for a `broadcast::Dynamic` handler to
answer it. Wire: a new lite-07-wip stream.

## Related

- [SUBSCRIBE_TRACKS](/quest/m1/archive/subscribe-tracks.md) - moq-transport answers and learns listings through this hook
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - the subscription shape each per-track SUBSCRIBE uses
