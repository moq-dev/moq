# Subscribe ranges

## Goal

A moq-lite-07 SUBSCRIBE replaces FETCH. One subscription asks for any number
of group ranges, past or live, bounded by group or by time, in ascending or
descending order, and the
publisher serves their union from one cursor without sending a group twice.
Every sequence in range either arrives or is named by a SUBSCRIBE_DROP, so
nothing blocks on a cache miss. Relays fill misses upstream with ranges, not
one request per group, including over moq-transport where group numbers may be
sparse. Lite FETCH is removed from lite-07.

## Plan

Decided in planning (09-29), after the fetch-span agent (#4558) found that a
relay bounds a sparse FETCH only if the model can request ranges:

- **Wire (lite-07, unpublished).** SUBSCRIBE carries a list of group ranges
  (an open end means live; ends may be relative, below) in place of Group Start/End, plus
  `order: asc | desc` (desc, newest first, is the default). Order landed in
  #5385; over moq-transport the upstream order is fixed at SUBSCRIBE, since no
  draft lets an update change it.
  Subscriber Max Age (the model's `max_delay`) stays as a cap alongside the
  ranges, since an untimed track's groups have no timestamp to bound. Decided 2026-10-10:
  historical requests also obey that cap and publisher max-age. An omitted
  FETCH budget means publisher max-age alone; translation to a range must
  not accidentally give it the live SUBSCRIBE zero-delay default. SUBSCRIBE_UPDATE
  replaces the list. Lite FETCH is removed from
  lite-07; published versions keep it, answered through the same model.
- **Holes.** SUBSCRIBE_DROP (restored in lite-07) names every sequence in the
  requested ranges the publisher won't deliver: never existed, expired, or
  refused upstream. Datagram groups are always dropped in a past range (#4982). The
  publisher delivers what it has and fills the rest as upstream answers.
- **Model.** `Subscription` gains `ranges` beside its `order`, mirroring the wire;
  `request_groups` as a separate API was rejected. A publisher's
  `track::Requests` receives a `track::Fill` per run of misses (landed), and
  `fetch_group` becomes a one-range subscription or is removed. This breaks published moq-net APIs.
- **moq-transport.** Some deployments use non-contiguous group numbers, so a
  relay never probes upstream one group at a time. Upstream, each run of
  locally missing groups becomes one standalone range FETCH, capped below the
  upstream's live group (which arrives via SUBSCRIBE), so it never blocks.
  moq-transport caps a FETCH at the Largest Object, so it can return the live
  group's existing prefix but never wait for its future objects. Downstream,
  an IETF FETCH is served from the model's ranges, ending at the Largest
  Object's group, which it reads whole even while open (maintainer,
  2026-10-25): the cache cannot know an open group's real Largest Object.
- **Relative ends** (maintainer, 2026-10-11, superseding the 10-07 `Live`
  flag and closing #5085). Each range end is `Absolute(group)` or
  `Relative(n)`, naming the group n before the latest; either end may be
  relative, and an omitted end is live. An offset past the track's start
  clamps to group 0. Before any group exists, relative ends resolve when the
  first one arrives, so `Relative(0)..` starts at it. The cross-language and
  bridge tests cover both edges. Ends are inclusive of whole groups:
  a relative start resolves to frame 0 of that group, and a relative end to
  just after its last frame (the model's exclusive `Position::after_group`),
  so `Absolute(0)..Relative(0)` includes the latest group. Live is `Relative(0)..`, so no separate `Live` field exists.
  Merging subscribers is the union of their ranges, so a range above the
  live edge never hides the latest group from a merged live subscriber,
  which is the starvation the flag existed to prevent. Subscriber Max Age
  still caps every range, so `Absolute(0)..` means everything not stale.
  A relay that cannot resolve a relative end locally forwards it upstream as
  relative.
- **Time ends** (maintainer, 2026-10-17, for DVR seek in the
  [archive line](/quest/m1/archive/README.md)). A range end may also be
  `Time(pts)`, in the track's timescale. Whoever holds the track resolves it:
  the cache from its groups' timestamps, or a `track::Requests` handler such as
  an archive consumer. A start resolves to the group containing pts, and an
  end to just after that group. A lite-07 relay that cannot resolve one
  forwards it upstream unchanged. An untimed track refuses a time end, and a
  moq-transport bridge resolves it locally or refuses it, since moq-transport
  has no time filter. It is part of lite-07 and must land before
  [lite-07 finalizes](/quest/m1/lite07-finalize.md), which this line gates. This amends the 09-29
  premise that ranges are group-only because a group's timestamp isn't always
  known: timed tracks know it, and untimed ones refuse. The cache half landed in its own PR after #5929.
  A datagram is judged on its own (maintainer, 2026-10-20): its timestamp
  against the range's times, its sequence against the absolute and resolved
  relative groups, never through the settle walk. Refusing time anchors on
  datagram tracks was considered and can't work: nothing signals that a track
  carries datagrams, and the lite draft lets a publisher send any single-frame
  group as a datagram, including a copy of one it also streams. Datagrams stay
  ephemeral (maintainer, 2026-10-20): a buffered datagram never indexes where
  its group starts, so a time end settles across a sequence that arrived only
  as a datagram, and a stream copy of that group arriving later is read even
  if it starts past the end.
- Ranges are whole groups: a range always starts at a group's first frame
  (maintainer, on [moq-dev/moq#6465](https://github.com/moq-dev/moq/pull/6465)). The IETF joining FETCH for the
  live group's uncached prefix stays, because today's bridge relies on it
  (maintainer, 09-29).
- The relay half of fetch-span moves here; #4558 landed only the no-handler
  skip.

This line owns the end-to-end test: a relay with a sparse cache answers a
wide past range plus live in both orders, over lite-07 and moq-transport, with
every sequence delivered or dropped.

### Delete the old start bookkeeping (2026-10-24 pre-cut audit)

Lite `TrackRun`'s fixes cluster on where a subscription starts and what it
is owed or dropped (e713c30991, 36f82ac450, 35fa44f6bf, e1b5d2741f,
f235fcfcbd, df0bd5c36f, 342445f251, 06765f4c1a, 4927167904, f52848d634,
0513c07369, 5ad3652871). That state spans about eight fields (`floor`,
`cap`, `start`, `first`, `skipped`, `served`, `start_frame`, `end_frame`),
plus `cursor_floor` and a pre-06 re-pin branch, encoding three wire
generations of "start". When ranges land:

- Move what each cursor is owed and what was dropped into the model, so
  `TrackRun` only translates it into SUBSCRIBE_START/DROP/END per version;
  delete `skipped`, `served`, `first`, and the pre-06 branches rather than
  layering lite-07 ranges on top.
- On lite-07 a stream or a DROP accounts for every sequence except one sent
  only as a datagram, which can be lost unseen
  (`a_lost_datagram_delays_the_end` in `tests/integration/track_tail.rs`).
  So the subscriber's end is "owed range covered and no stream active",
  except that a hole a datagram may have left still settles within a bounded
  grace; only reliable accounting of datagram sequences could remove that.
  Keep `Settle`'s full grace for lite-05/06 and moq-transport behind one
  version predicate.
- Lite-07 is unpublished, so sending a fresh START per update can remove the
  `requested == start` gate (`lite/subscriber.rs` about line 7366).

## Required

- [Linear fill path](/quest/m1/subscribe-ranges/fill-linear.md) - filling stays flat as range count and span grow
- [Fetch as a range](/quest/m1/subscribe-ranges/fetch-range.md) - `fetch_group` becomes a one-range subscription that ends once its range is accounted for
- [Lite-07 ranges](/quest/m1/subscribe-ranges/lite.md) - the lite-07 wire, Rust publisher and subscriber, and the draft
- [moq-transport range updates](/quest/m1/subscribe-ranges/ietf.md) - widening and narrowing upstream ranges, and `Time` ends
- [JS ranges](/quest/m1/subscribe-ranges/js.md) - `@moq/net` model and lite-07 parity
- [Range count cost](/quest/m1/subscribe-ranges/range-cost.md) - a subscription's range count cannot multiply per-group cost

## Related

- [IETF fill timeout](/quest/m1/ietf-fill-timeout.md) - bounds upstream waiting independently of content age
