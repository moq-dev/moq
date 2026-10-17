# [XL] SUBSCRIBE_ALL

## Goal

A moq-lite-07 SUBSCRIBE_ALL subscribes to every track of every broadcast under
a path prefix. The publisher announces each track it produces, naming its
broadcast and carrying its TRACK_INFO, including tracks created after the
request and broadcasts that appear later, and delivers their groups; a
broadcast's tracks end when it ends. It never creates a track the publisher
serves only on request (an any-name `broadcast::Dynamic` handler such as a
transcoder). A relay answers many downstream SUBSCRIBE_ALLs from one upstream
request. Rust and JS both speak it.

## Plan

Decided by the maintainer on 2026-10-17:

- One request, mirroring IETF SUBSCRIBE_TRACKS, so a new track's first groups
  arrive on the same request that announced it. Rejected: a track-listing
  stream plus a SUBSCRIBE per track (two mechanisms, and a race on a new
  track's first groups) and a name filter (no consumer needs one yet).
- Prefix-scoped, like SUBSCRIBE_TRACKS, so
  [SUBSCRIBE_TRACKS and PUBLISH](/quest/m1/archive/subscribe-tracks.md) maps
  onto it 1:1 and moq-lite and IETF clients share one mechanism. A prefix
  naming exactly one broadcast is the single-broadcast case. Rejected: a
  per-broadcast request with a fan-out per announced broadcast, and
  exact-namespace only.
- Gates the lite-07 cut ([Finalize moq-lite-07](/quest/m1/lite07-finalize.md));
  target `moq-lite-07-wip`. Older versions refuse the request; no emulation
  over SUBSCRIBE.
- The model gains one hook on `origin::Consumer` (or wherever prefix
  announcements live) that yields each broadcast under the prefix as it
  appears and a `track::Consumer` for every track its producer creates.
  SUBSCRIBE_TRACKS uses the same hook.

Guidance:

- A broadcast is a path and its epoch: a new epoch at the same path is a new
  broadcast, and each delivered track names which one it belongs to.
- A track announced here is the same cached track an ordinary SUBSCRIBE gets:
  a relay never holds two copies, and a track already subscribed upstream
  joins the request rather than duplicating it. Overlapping prefixes from
  different downstreams share upstream state where they overlap.
- Subscription parameters (priority, order, max delay, and later ranges)
  apply to every track; per-track updates are not needed yet.
- Name how a track's end, a broadcast's end, and a refused track surface, and
  how SUBSCRIBE_DROP applies per track.
- Wire: the request names the prefix; the reply announces broadcasts and
  tracks with identifiers that group streams reference. Update
  `drafts/draft-lcurley-moq-lite.md` and its lite-07 changelog, then run
  `just drafts check` and `just test interop --all`.
- Auth: the request needs subscribe permission on the prefix, as
  announcement discovery does; a broadcast outside the grant is never
  delivered.
- Fan-out: benchmark downstream requests against broadcast and track count per
  the AGENTS.md fan-out rule.
- Split the JS half into its own quest if the PR grows too large to review.

Test on mock time, directly and through a relay: tracks present at the
request, a track added later (its first group arrives), a broadcast appearing
and ending under the prefix, a new epoch at the same path, a dynamic-only track
never created, and two downstream requests sharing one upstream request.

Public API: the new prefix hook in Rust and `@moq/net`, mirrored names. Wire:
a new lite-07-wip request.

## Related

- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - the subscription shape whose parameters this applies to every track
