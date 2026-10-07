# [M] lite-07 Latest flag

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

- lite-07 SUBSCRIBE and SUBSCRIBE_UPDATE carry a separate `Latest` boolean.
  `Group Start` and `Frame Start` become a plain absolute floor, with no
  "0 means none" sentinel.
- `Latest: true` replaces the floor with the latest group when the latest group
  is smaller, starting at frame 0 of that group (its keyframe). `Frame Start`
  only qualifies an explicit floor group.
- The model mirrors the wire: `Subscription { floor: (group, frame), latest }`
  in Rust and `@moq/net`, replacing the `Option` floor.
- Merging across subscribers (`Subscription::poll_combined`, JS
  `combineSubscriptions`): `latest` is the OR, and the floor is the minimum
  `(group, frame)`. Each subscriber's own cursor still filters what it sees.
- Older wires map at the codec only, with no change to a published version:
  lite-06 `Group Start` 0 is `latest`, any other value a floor; pre-06 absent
  is `latest`; moq-transport `Largest Object` and `Next Group Start` filters are
  `latest`, `Absolute Start` a floor.
- When merged subscriptions need both the latest group and a floor that an
  older upstream wire cannot express in one SUBSCRIBE, the relay MAY first ask
  TRACK_STATUS for the largest group and subscribe from the lower of the two.
- Docs stay inline: the lite draft (field, semantics, lite-07 changelog),
  `doc/concept`, and the Rust and JS API docs.

Test with mock time: the starvation case (a floor-4 subscriber and a `latest`
subscriber on a quiet track whose newest group is 3) in-process and through a
relay on lite-06 and lite-07-wip, a frame floor merged with `latest`, and the
codec mapping of each older wire.

## Related

- [Late lower group](/quest/m1/lite-late-lower-group.md) - requires this; #5000 rebases onto it and drops its own floor-combining change
- [SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md) - names undelivered groups once publishers report them
