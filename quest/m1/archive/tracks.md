# [L] TRACKS

## Goal

A moq-lite-07 TRACKS request lists the tracks of one broadcast as its
producer creates them, each with its name and TRACK_INFO, marks when the
lister has caught up with every track that existed when it asked, and names
each track that no longer exists. The subscriber then SUBSCRIBEs to each track it wants, so
delivery stays pull: nothing is ever pushed into a pulled copy. A listing
never creates a track the publisher serves only on request (an any-name
`broadcast::Dynamic` handler such as a transcoder). A relay answers many
downstream TRACKS for a broadcast from one upstream TRACKS. Rust and JS both
speak it.

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
  anyway.
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
two: ANNOUNCE for broadcasts, TRACKS per announced broadcast. TRACKS names a
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
  properties and no name or stream-specific field. What differs is only what
  wraps it: the name, and an implicit ID assigned per entry, as ANNOUNCE_START
  assigns Announce IDs. A lister MAY skip the Track Stream for a track it
  learned this way, since the properties are the same immutable ones.
- **Caught up.** Mirror ANNOUNCE_OK's `Active Count`: the reply opens with a
  count of the tracks in the initial set, sent first, so the lister knows when
  it holds every track this hop knew when it asked; entries past the count are
  live. Caught up means caught up with this hop, decided by the maintainer on
  2026-10-19: a relay answers at once with what it currently holds and never
  waits on its upstream, since relays never block on peers. A cold relay may
  report zero; the upstream's tracks then arrive as live adds.
- **Track removed.** A named event referencing the entry's ID, sent when the
  publisher no longer serves the name (it left the broadcast's track table),
  so a SUBSCRIBE for it would now be refused. A track created again under the
  name gets a new entry and ID. Removing an ID that is not live is a protocol
  violation, as on the Announce stream.

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
    can send. Held state is the live set, never the history, and a
    downstream cursor reads the current set rather than a log: an add and its
    removal that a slow reader has not reached collapse to nothing, so churn
    costs a relay per-event work across its listings of that broadcast (the
    same fan-out an Announce stream pays) but no backlog. Implicit IDs keep
    growing, which costs a varint byte now and then, not state.
  - A hostile subscriber can open many TRACKS. Each takes a subscription slot,
    downstream listings of one broadcast share one upstream TRACKS, and a
    downstream reader is a cursor into the shared listing, so a slow or stalled
    reader costs flow control, not a copy of the listing.
- Fan-out: benchmark downstream listings against tracks per broadcast and
  broadcasts per relay, per the AGENTS.md fan-out rule; a new track's cost
  must not grow with the broadcast count.
- Split the JS half into its own quest if the PR grows too large to review.

Test on mock time, directly and through a relay: tracks present at the
request and the caught-up count matching them, a cold relay answering
caught up at once with what it holds and the upstream's tracks following as
live adds, a track added later (a
SUBSCRIBE from the listing gets its first group from the cache), a track
removed and created again under its name (a new ID), add and remove churn
collapsing for a stalled reader, a removal of an unknown ID refused, the
broadcast
ending the listing, a new epoch at the same path not continuing an old
listing, a dynamic-only track never created, two downstream listings sharing
one upstream TRACKS, and the `announces` cap resetting an oversized listing.

Public API: a track-listing hook on `broadcast::Consumer` in Rust and
`@moq/net`, mirrored names, and a way for a `broadcast::Dynamic` handler to
answer it. Wire: a new lite-07-wip stream.

## Related

- [SUBSCRIBE_TRACKS](/quest/m1/archive/subscribe-tracks.md) - moq-transport answers and learns listings through this hook
- [SUBSCRIBE_OK names where delivery starts](/quest/m1/subscribe-ok-start.md) - a warm relay answers like a cold one, found while building SUBSCRIBE_ALL
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - the subscription shape each per-track SUBSCRIBE uses
