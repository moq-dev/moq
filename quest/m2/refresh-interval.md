# [L] Refresh a track by interval, and on request

## Goal

A publisher can declare in TRACK_INFO a refresh interval: a live join asks
for a new group only once the newest group arrived at least that long ago.
A reader can also ask for one with a public, no-argument `refresh()` on the
track subscriber, in Rust and `@moq/net`, judged by the same interval. A
request says only "a group newer than the newest this node holds": it
raises SUBSCRIBE_UPDATE's existing `Wanted Group` and adds no field, no
request number, and no tag on groups. Every hop coalesces requests and
judges them against its own cache, so the publisher's interval caps how
often new groups are made, by design rather than by a per-hop rate limiter.

The interval is a duration, encoded and read like `max_age`, so it means the
same on a timed and a scale-less track. A group's age is measured on the
judging node's own monotonic clock from when that node first received (or, at
the publisher, created) the group, as arrival-age expiry already measures it.
It never compares media timestamps, which carry no wall-clock meaning and, on
a scale-less track, no unit, and it adds no local clock to the model.

A track of one-frame groups, such as a [thumbnail](/quest/m4/hang-thumbnail.md),
then produces a fresh group when a reader arrives or asks, and never more
often than its interval, however many readers fan out behind the relays.
A track with no group yet needs no interval: its first reader asks for the
first group at once, and the interval applies from that group on.

On the publishing side `track::Demand::wanted()` is a boolean, like every
other `wanted`, beside one refresh signal that says a new group is asked
for.

## Plan

Planned 2026-11-13 (maintainer, /quest-plan of a remote desktop for
kixel.ai). The consumer is Kixel's desktop host, which serves an agent's
screenshot from a lossless thumbnail and wants dashboard previews from the
same track.

Decided 2026-11-13:

- ✅ moq m2, in lite-07 (rejected: m4 and lite-08). Its consumer is later
  work, which would put it in m4, but it changes lite-07-wip, and missing
  that freeze costs a whole wire version.
- ✅ An interval in TRACK_INFO plus a public `refresh()` (rejected: changing
  only the byte-ratio check; a per-hop rate limiter). The interval bounds
  the request rate by what the publisher declares.

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ `refresh()` takes no argument and raises `Wanted Group`, which
  [time addressing](https://github.com/moq-dev/moq/pull/8908) makes a start
  time: the node asks for a group that starts after the newest it holds
  (rejected: a request counter riding SUBSCRIBE_UPDATE's update number,
  with every group tagged by the highest update its publisher had seen,
  which was this quest's 2026-11-13 recommendation; a caller-supplied
  timestamp argument now; the interval alone with no `refresh()`). The
  counter rode a field deleted the day after it was recommended: on
  2026-11-14 OLD drops became final and `Update` left lite-07. Rebuilding
  per-hop numbering and a tag on GROUP for one reader costs more wire than
  it earns.
- ✅ No timestamp argument yet. The maintainer's use is a thumbnail on
  demand and, later, a robot that asks before each frame, like a PLI
  (WebRTC's "send me a keyframe") with a way to deduplicate requests.
  `Wanted Group` already deduplicates: every request made while the same
  group is newest raises it to the same value, so a hop forwards at most
  one update per group, and a publisher ignores a value its newest group
  already passes. An argument ("a group at or after this time") can be
  added later without a wire change, since the field is already a time.
- ✅ `track::Demand::wanted()` returns a boolean and a separate signal
  reports a pending refresh (rejected: keep `Option<u64>` and add
  `refresh_pending()`; keep). This quest takes that break because it is
  already changing this API.

What the guarantee is, and is not: `refresh()` resolves with the first
group that starts after the newest group the node held when it was called.
A group already in flight toward the node, created before the request
reached the publisher, satisfies it. So "a still taken after my action" is
best effort across hops: it holds at the publisher itself, and a remote
caller that needs it puts its own marker in the frame and reads until it
sees it. The [thumbnail](/quest/m4/hang-thumbnail.md) promises that much
and no more. The interval may delay the new group, never substitute an
older one. Rust and `@moq/net` share this contract.

Facts found while planning and by the audit:

- `Refresh` is a byte ratio today. `over_refresh`
  (`rs/moq-net/src/model/group.rs`) tests `cache - first > first * ratio`,
  so a complete one-frame group never refreshes at any ratio.
- `Subscriber::join` is `pub(crate)` in `rs/moq-net/src/model/track.rs`.
  The only trigger is a live-edge subscribe, and no reader can ask for a
  group.
- SUBSCRIBE_UPDATE's `Wanted Group` already carries a relay's coalesced
  demand upstream: "the newest group the subscriber's live-edge joins want
  ... A publisher MUST ignore a value at or below its largest group" (the
  draft's SUBSCRIBE_UPDATE).
- `track::Demand::wanted()` returns `Option<u64>`
  (`rs/moq-net/src/model/track.rs:5725`) where `broadcast::Demand`,
  `group::Demand`, and `bandwidth::Demand` return `bool`. Callers outside
  moq-net either test `.is_some()` (moq-transcode, moq-video, moq-ffi,
  moq-stats) or compare it with their own largest group to detect a
  pending refresh, written out three times (`rs/moq-json/src/refresh.rs`,
  `rs/moq-stats/src/produce.rs`, `rs/moq-video/src/encode/producer.rs`).
  Since groups are keyed by start the number is "one past every key
  written", which is not the start the next group will have. JS mirrors it
  as `get wanted(): Getter<bigint | undefined>` (`js/net/src/track.ts`).
- `rs/moq-video/src/encode/producer.rs` forces a keyframe on a join; it
  keeps working on the byte ratio.

Recommendations for the PR:

- Whether the interval sits beside the byte ratio or replaces `Refresh` with
  a typed choice. A track may want both (video wants a keyframe when the GOP
  is long, and not more than once a second), so recommend both, either one
  firing within the interval.
- The refresh signal's shape on `Demand`: recommend one awaitable that
  resolves each time a new group is asked for and is cleared by the next
  `append_group`, so the three hand-written comparisons become one call and
  no caller sees a group number. `FillGroups::wanted()` returns `Option<u64>`
  with a third meaning (the next group to serve); leave it, or rename it in
  the same PR if it reads as the same thing.
- Mirror `wanted` and the signal in `@moq/net` and the bindings' demand
  handle, with MIGRATE notes: this is the breaking part.
- Update the lite draft (TRACK_INFO's interval, `Wanted Group`'s text, and
  the changelog), `doc/concept/moq-lite.md`, and the matching JS. Run
  `just test interop --all`.
- `Wanted Group` is `Wanted Start` since [time addressing](https://github.com/moq-dev/moq/pull/8908):
  the wanted group's start plus one.

Tests: a relay chain where many readers join and call `refresh()` on a
one-frame track produces one group per interval at the publisher, with time
mocked; repeated `refresh()` calls while one group is newest send one
SUBSCRIBE_UPDATE per hop; a publisher whose `Demand` reports a refresh opens
exactly one group for it.

Public API: breaking for `track::Demand::wanted()` in Rust, `@moq/net`, and
the bindings; additive otherwise (`refresh()`, the interval on `Info`).
Wire: lite-07-wip TRACK_INFO gains the interval; SUBSCRIBE_UPDATE keeps
`Wanted Group` and gains nothing.

## Related

- [Hang thumbnails](/quest/m4/hang-thumbnail.md) - the one-frame track this refreshes
