# [M] A route moving behind one next hop resends no group

## Goal

When a relay's route moves to another announcing relay behind the same next
hop, the subscription it opens on that hop starts after the newest group its
reader handed out, so no group crosses the link twice.
`route_audit::failures::a_route_moving_behind_one_next_hop_resends_a_group`
and `failures_at_one_instant` in `rs/moq-net/tests/integration/route_audit/`
assert that no link carries a group twice.

## Plan

Found by [the uncovered cases](https://github.com/moq-dev/moq/pull/9111): in 2
of 8 seeded 12-relay meshes with simultaneous failures, one group (a whole
GOP for video) crossed a link twice.

Root cause: routes are per announcing relay, and a lite-07 request names the
routing node its first hop chose. So when `A`'s route moves from `X` to `Y`,
`B`'s front changes copies although its next hop, `A`, is the same session,
and opens a second SUBSCRIBE on it. The new copy starts at the newest group
the reader handed out, inclusive: `Reader::sync` makes `self.newest` the
copy's floor (`rs/moq-net/src/model/resume.rs`), and `clamped` turns it into
`Anchor::Time(floor)` (`rs/moq-net/src/model/subscription.rs`). `B`'s reader
skips the repeat, but its bytes crossed `A`-`B` twice. It is inclusive because
a lite-07 range cannot say "after group K": a time anchor names the group
containing that time, the next group's start is unknown on a timed track,
and a start partway through a group is refused on lite-07
(`wire_anchor` in `rs/moq-net/src/lite/subscribe.rs`). Lite-06 could bound
frames this way. A switch to another neighbour sends the group over another
link, so no single link carries it twice, though the bytes are still
redundant.

Decided 2026-11-19 (maintainer, in the coordinating session, on
[moq-dev/moq#9111](https://github.com/moq-dev/moq/pull/9111)): ✅ option 1,
a SUBSCRIBE range may start at an absolute group and frame position on
lite-07-wip (rejected: 2, the partial fix with no wire change; 4, accepting
the duplicate; 3 stays rejected). It lands before the cut. Warm routing
moves pulls for price, so these switches become common once it ships.

Options, as they were put:

1. Wire, on lite-07-wip: a SUBSCRIBE range start may be an absolute
   (group, frame) position again, as on lite-06. The GROUP header already
   carries `frame_start`. A switch then asks for group K from frame N, N the
   frames already handed out: nothing for a finished group, only the unread
   tail of one in flight. That also stops a recovered partial group from
   crossing whole. Recommended, and decided before the cut, since the wire
   freezes then. Updates `js/net` and the lite draft in the same PR.
2. No wire change, partial: when the newest handed-out group finished in the
   replaced copy and its end is known (untimed tracks with consecutive keys,
   or groups finished with `finish_at`), floor at the next key or the end.
   Local to `Reader::sync`, but timed tracks closed with plain `finish()`
   keep the duplicate.
3. Keep the existing subscription when only the announcing relay behind the
   same next-hop session changes. Rejected: it conflicts with a request
   naming its first hop's routing node, which keeps requests loop-free.
4. Accept one group per same-next-hop switch and document the cost.

