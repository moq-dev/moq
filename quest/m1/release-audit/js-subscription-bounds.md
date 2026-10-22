# [M] JS honors a subscription's group bounds

## Goal

A `@moq/net` subscription's `groups` bounds decide what reaches it on every
path, as in Rust: each reader's reliable reads, its live-edge choice, its
updates, and the datagrams a JS publisher sends for it. A reader asking for
`[1, 3)` gets groups 1 and 2, whatever another reader of the same track asked.

## Plan

Two P2 defects, both inherited from release.

**Reader side** (2026-10-22 subscription audit, main 1f5caa7026). With groups
0..4 cached and a second reader asking for everything, a reader asking for
`[1, 3)` received [1, 2, 3, 4]. Updating a reader to `[3, 4)` still returned
[0, 1, 2, 3, 4]. A zero-delay reader capped below group 2 received group 4
rather than its own latest eligible group. `Subscriber`'s constructor in
`js/net/src/track.ts` seeds its cursor with the floor only, dropping the end,
and `Subscriber.update` changes upstream demand without re-bounding the
cursor. Apply each reader's bounds to its own cursor, independent of the
aggregate upstream demand, and re-resolve them on update as Rust does, never
redelivering a group already read. The `Subscription.groups` doc ("a floor,
not a request") then needs to name the end too.

**Serving side** (moved here from [JS ranges](/quest/m1/subscribe-ranges/js.md),
decided 2026-10-22 so a reproduced defect in published behavior does not wait
on the lite-07 range chain). At main 1d43c096e0, for an inclusive request from
(group 0, object 1) through (group 1, object 0), publishing datagrams at
(0, 0), (1, 0), and (2, 0) should deliver only group 1. The real JS publisher
with paired mock transports delivered groups [0, 1, 2] on IETF drafts 20/22
and [0, 1] on Lite 06/07. IETF's datagram task receives no resolved range,
losing both bounds; Lite installs the group cap but does not pass frame
bounds to its datagram task. The three reliable-group range controls passed.
Lite's defect is inherited; JS IETF datagrams newly expose it after
[moq-dev/moq#4979](https://github.com/moq-dev/moq/pull/4979).

One selection rule for reliable groups and datagrams. Test on both object
boundaries and group ranges, plus updates that narrow and widen, against an
aggregate wider than the reader. Correcting an accepted filter needs no API or
wire change.

## Related

- [JS ranges](/quest/m1/subscribe-ranges/js.md) - replaces `groups` with range lists later; builds on this rule
