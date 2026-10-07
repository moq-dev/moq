# [M] lite-07 Live flag

## Goal

Subscribers with different group and frame floors that share one upstream
subscription, in-process or through a relay, each receive everything their own
floor and `Subscriber Max Age` allow. No subscriber's floor starves another:
a resumed subscriber whose floor sits above the live edge never hides the
latest group from a subscriber that wants it.

Today a floor and "no floor" are one field. On lite-06 `Group Start` 0 means no
floor, and the model keeps a separate `None` from pre-06's "absent means the
latest group". Merging an explicit floor with `None` either drops the late
group the floor asked for (main) or starves the floorless subscriber until the
floor's group exists, which on a quiet catalog track is never (#5000, the
failure #4940 fixed from the other side).

## Plan

Decided (maintainer, 2026-10-07):

- lite-07 SUBSCRIBE and SUBSCRIBE_UPDATE carry a separate `Live` boolean.
  `Group Start` and `Frame Start` become a plain absolute floor, with no
  "0 means none" sentinel.
- A live-only subscriber has no floor, the neutral value for the minimum, so
  lite-07 makes the floor optional. A subscription with neither a floor nor
  `Live` is refused.
- `Live: true` lowers the floor to frame 0 of the oldest group that
  `Subscriber Max Age` has not expired, when that position sorts below the
  floor. That is the start lite-06 already resolves for `Group Start` 0, so a
  buffering subscriber keeps its backlog, and a max age of 0 still means the
  latest group. Comparing positions, not groups, means a merged floor of
  (3, 5) with group 3 as the latest still delivers frames 0-4.
- The model mirrors the wire: `Subscription { floor: Option<(group, frame)>,
  live }` in Rust and `@moq/net`.
- Merging across subscribers (`Subscription::poll_combined`, JS
  `combineSubscriptions`): `live` is the OR, and the floor is the minimum
  `(group, frame)`. Each subscriber's own cursor still filters what it sees.
- Older wires map at the codec only, with no change to a published version:
  lite-06 `Group Start` 0 with `Frame Start` 0 is `live`, any other pair a
  floor (`(0, N)` is a catalog resume); pre-06 absent is `live`.
  moq-transport has no current-group filter, so `live` is the bridge
  `ietf::subscriber::subscribe_join` already sends: `Largest Object` plus the
  current group's prefix, from a `Relative(1)` fill on draft-20+ or a relative
  joining FETCH on older drafts. `Next Group Start` (draft-20 `Relative(0)`)
  keeps its draft meaning, a floor at the group after Largest Object; a bare
  `Largest Object` is a floor at the next object; draft-20 `Relative(1)` is
  `live`; `Absolute Start` is a floor.
- When merged subscriptions need both `live` and a floor that an older
  upstream wire cannot express in one SUBSCRIBE, a moq-transport upstream MAY
  first be asked TRACK_STATUS for the largest group, subscribing from the lower
  of the two. For an empty track the `live` mapping alone covers both, since
  moq-transport starts it at {0, 0}. moq-lite has no
  TRACK_STATUS, so a lite-06 or pre-06 upstream is subscribed at floor 0 and
  the relay filters locally, as main does today.
- Docs stay inline: the lite draft (field, semantics, lite-07 changelog),
  `doc/concept`, and the Rust and JS API docs.

Test with mock time: the starvation case (a floor-4 subscriber and a `live`
subscriber on a quiet track whose newest group is 3) in-process and through a
relay on lite-06 and lite-07-wip, a buffering `live` subscriber (max age > 0)
merged with a floor above the live edge, a frame floor merged with `live`, and
the codec mapping of each older wire.

## Related

- [Late lower group](/quest/m1/lite-late-lower-group.md) - requires this; #5000 rebases onto it and drops its own floor-combining change
- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - names undelivered groups once publishers report them
