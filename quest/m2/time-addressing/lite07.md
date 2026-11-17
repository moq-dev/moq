# [L] The lite-07 wire addresses groups by start

## Goal

moq-lite-07 carries no group sequence numbers: a GROUP names its start and
its predecessor's start, GROUP_END says where a whole group ends, and every
subscription bound and drop is a time. Datagrams keep a sequence of their own on their own track kind. Rust speaks it, the lite
draft specifies it, and lite-01..06 are unchanged.

The same rewrite carries the smaller lite-07 fixes the pre-cut audit decided
on 2026-11-16, listed in the Plan: an encoding for "no limit" in
`Subscriber Max Age`, GROUP without `Frame Start`, no Padding stream or
datagram, and a track listing that says each track's kind.

## Plan

Decided 2026-10-25 (maintainer), from the spike:

- **GROUP** carries the start and the previous group's start, absent for the
  first group of an epoch, so a cache or reader sees a hole without a round
  trip.
- **GROUP_END** carries the group's exclusive end timestamp, meaning the
  group is whole. A bare FIN means an incomplete group whose frames all
  arrived, and a reset aborts. A relay closes an expired or partial group
  instead of resetting it, which changes the draft's reset rule for those
  groups (around line 760).
- SUBSCRIBE and SUBSCRIBE_UPDATE bounds, SUBSCRIBE_OK's largest and its Range
  Starts, SUBSCRIBE_STARTED, SUBSCRIBE_FINISH, SUBSCRIBE_SKIP, and SUBSCRIBE_DROP
  address by start. Datagrams keep their own sequence (maintainer, 2026-11-01). Drops
  name time spans with no group to deliver, not sequence ranges. `Relative(n)`
  stays a group count.
- Positions stay (start, frame index), preserving transport frame offsets.
- SUBSCRIBE_UPDATE's `Wanted Group` (a sequence plus one) and SUBSCRIBE_OK's
  `Group` and `Largest Group` are sequences too, and change with the rest.
- `Timescale` 0 stays and means scale-less: keys and frame timestamps are
  bare numbers, and a scale-less frame carries its group's key (decided
  2026-11-03, maintainer: ingested legacy tracks with bare keys still cross
  lite-07; see [the model](https://github.com/moq-dev/moq/blob/7d1bee79cb8fcb4d816fc53f0547ef8b2b19756a/quest/m2/time-addressing/model.md)). FRAME and
  datagrams always carry a timestamp. The untimed cases of
  TIMESTAMP_MISMATCH, Time anchors, and arrival-age-only expiry become the
  scale-less cases. The rule that a Group's frame
  0 not start before a lower-sequence Group's becomes the strictly
  increasing start.

SUBSCRIBE_OK also says whether the newest group it reports has already ended
(finished or closed), so a reader joining a cached track knows at once
whether that group is the live one. Added 2026-11-04 (maintainer) from
Kixel's T3 readers, which today infer it by waiting a moment for the group's end after
its last frame, because the end only follows the last frame. It rides this
quest's SUBSCRIBE_OK change instead of a later wire bump.

Update `drafts/draft-lcurley-moq-lite.md` (fields, the group-end rules, and the
lite-07 changelog) and `doc/concept/moq-lite.md`, and run `just drafts check`
and `just test interop --all`.

Also reword `drafts/draft-lcurley-moq-e2ee.md`, which still calls the nonce's
group input a sequence (its group-allocation rule, about line 268): it is the
group's start, strictly increasing. The nonce bytes don't change, since both
the Rust and JS cores already feed it the group's wire ID, which
[the model](https://github.com/moq-dev/moq/blob/7d1bee79cb8fcb4d816fc53f0547ef8b2b19756a/quest/m2/time-addressing/model.md) makes the start. Folded in
2026-11-14 from the removed E2EE nonce quest (the m2 triage before the cut,
under /quest-afk, for the maintainer's review), since this PR's rename
outdates that wording.

Decided 2026-11-05 (maintainer, /quest-audit): ✅ this waits for
Lite-07 FETCH is single-group only ([moq-dev/moq#8616](https://github.com/moq-dev/moq/pull/8616)),
which rewrites the same lite-07 draft sections (`Range`, Positions,
SUBSCRIBE anchors).

Found 2026-11-14 by the [feature review](/quest/m3/release/feature-review.md):
reopening stale (OLD) drops is overbuilt for what it serves. Every
SUBSCRIBE_UPDATE reopens every OLD drop, and a relay settles a received stale
drop only for readers within budget and range; it took 27 recorded decisions
in [moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670), then
[moq-dev/moq#8546](https://github.com/moq-dev/moq/pull/8546),
[moq-dev/moq#8551](https://github.com/moq-dev/moq/pull/8551), and
[moq-dev/moq#8584](https://github.com/moq-dev/moq/pull/8584), and still has
an open model finding (`StreamWins.cfg`, which the finished-across-updates
quest owned until it was deleted on 2026-11-16: its premise was this
reopen). The case it serves is a relay reader whose budget widens after the upstream
dropped a group as OLD. Since this quest rewrites SUBSCRIBE_DROP,
SUBSCRIBE_UPDATE, and SUBSCRIBE_SKIP anyway, it can make OLD final on its
subscription: the widening reader asks with a SUBSCRIBE of its own, as the
draft already requires for delivered or finally dropped groups, and the
`Update` numbers, re-offers, reopen rules, and their model configs go. Also
check whether GROUP's previous-start link makes SUBSCRIBE_SKIP redundant.
The review wrote that YIELDED continuations would keep their hold; on
2026-11-16 they left lite-07 too ([YIELDED deferred](/quest/m2/yield-defer.md)),
so after both PRs a group has a second stream on one subscription only as
a duplicate, and no "held unread until the earlier stream ends" rule
remains for this rewrite to preserve.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (a) OLD drops are final on their subscription: this rewrite deletes `Update`, the reopen and re-offer paths, and their model configs (rejected: (b) keep reopening and finish the finished-across-updates quest, deleted 2026-11-16).

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit). These
ride this PR because it already rewrites the messages they touch, and each
is free only until lite-07 freezes:

- ✅ SUBSCRIBE and SUBSCRIBE_UPDATE encode `Subscriber Max Age` as
  milliseconds plus one, 0 meaning no limit (rejected: write 2^53 - 1 into
  the draft as "no limit"; leave it). FETCH's field of the same name and
  TRACK_INFO's `Publisher Max Age` already do. Today Rust sends
  `MAX_SAFE_DELAY_MS = 2^53 - 1` for `Delay::max == None`
  (`rs/moq-net/src/lite/publisher.rs:119`, `lite/subscriber.rs:12354`) and
  JS sends `NO_MAX_DELAY = Varint.MAX_U53` and reads it back by comparison
  (`js/net/src/internal.ts:41`, `js/net/src/fill.ts:626`). The draft never
  names that value, and a third implementation that picks a larger
  "forever" is refused by JS's 53-bit read. Both constants go on lite-07.
- ✅ GROUP loses `Frame Start` (rejected: keep it and state the one case
  that sets it). A subscription delivers whole groups, FETCH has no GROUP
  header, and the last case that set it, a continuation, leaves with
  [YIELDED](/quest/m2/yield-defer.md). The Rust publisher says "A lite-07
  range covers whole groups, so it has none", and JS sets `frameStart` only
  from a lite-06 SUBSCRIBE's bound. Before deleting it, grep both
  publishers for a lite-07 path that opens a Group Stream past frame 0; if
  yield-defer has not landed, the continuation is one, and the field goes
  with whichever PR lands second. lite-06 keeps its field.
- ✅ The Padding Stream (0x2) and Padding Datagram (0x1) types are removed;
  the datagram `Type` stays (rejected: keep the datagram one, as the
  2026-10-30 decision preferred; keep both). Both are received and
  discarded, nothing sends either outside a test and a benchmark, and their
  one planned sender is parked with no named consumer
  ([probe padding](/quest/m5/probe-padding.md), which would add its type
  back in a later version or an extension). An unknown
  stream type is reset and an unknown datagram type discarded, so adding
  either back later touches no message a lite-07 peer parses.
- ✅ The track listing carries each track's kind, so a datagram track can be
  listed (rejected: datagram tracks move to an extension draft; listings
  never show them). Datagram tracks stay in core with everything they have
  today, including the lite-05/06 group-datagram bridge and the bindings: a
  named consumer wants them. Today "TRACK_INFO does not say which kind a
  Track is", so `a_datagram_track_is_never_listed`
  (`rs/moq-net/src/model/listing.rs`). Recommended shape: TRACKS_ADD gains a
  `Kind` before its info, which is then a TRACK_INFO or a DATAGRAMS_INFO;
  the model's listing entry, `@moq/net`, and the bindings' listing report
  the kind, which is additive there.

- ✅ SUBSCRIBE_DROP names a span and carries no group count (rejected: a
  count, which would also have let SUBSCRIBE_SKIP become a drop with a
  count of 0). Today's `Group Count` is a count of keys, so it becomes the
  span's end. `track::Ordered`'s `Gap` reports the span.

Recommendations for the same PR, not decided:

- GROUP_END takes the value FRAME reserves, a `Payload Length` of 0,
  followed by the end timestamp. [Takeover](/quest/m3/group-takeover.md)
  planned a marker with the same name and the same reserved value that
  pauses a group instead; it now designs its own marker in lite-08.
- Anchor types 1 (Absolute) and 3 (Time) are aliases once a group's key is
  its start: the publisher maps both to `Anchor::Time`, and the model
  already has only `Oldest`, `Relative`, and `Time`. Keep one type with one
  end rule (today type 1's end is exclusive and type 3's inclusive), and
  read it with the full 2^61 width in JS, which decodes a Time anchor with
  `u53()` today.
- GROUP's previous start is optional: encode it plus one with 0 for none,
  as the draft's other optional integers are.
- SUBSCRIBE_SKIP stays for spans a subscriber can't infer, such as the tail
  of a bounded Range where starts are sparse, and GROUP's previous-start
  link carries the gap between two delivered groups, so the Subscribe
  Stream no longer carries one SKIP per group (`append_group` skips the
  keys between appends, and lite-07 forwards each today). With no count
  on a drop, a skip and a drop stay two messages.
- Delete the draft's binding 5 (Qmux over RTCDataChannel): its table row,
  its two paragraphs, its changelog line, and the DTLS clause in Security
  Considerations. [moq-dev/moq#8771](https://github.com/moq-dev/moq/pull/8771)
  removed the implementation with peer-to-peer.
- At the rename to `moq-lite-07`, delete what describes earlier versions'
  encodings in the body and in the lite-07 changelog, which itself says
  "Removed rules for mapping onto earlier versions": the changelog lines
  about what moq-lite-05 and moq-lite-06 do with a `Timescale` of 0 and
  with SUBSCRIBE_DROP bounds. The 2^61 timestamp bound stays, and its
  "so every version can carry it" is its only reason.

Kept as they are, so this rewrite leaves them alone: every lite version
from 01 to 06 keeps negotiating with today's behaviour, and a path segment
starting with `.` still hides a claim, with no typed flag (both decided
2026-11-16; AGENTS.md records the dot as the exception to its
reserved-prefix rule). Not reopened by the audit: AUTH_ERROR's retry codes
(the 2026-11-14 pick stands), and AUTH_OK's rate fields, which wait for
lite-08 with [the rate grant](/quest/m5/rate-grant.md).

Public API: none beyond [the model](https://github.com/moq-dev/moq/blob/7d1bee79cb8fcb4d816fc53f0547ef8b2b19756a/quest/m2/time-addressing/model.md).
`@moq/net` and the bindings' track listing gain a kind. Wire: lite-07-wip
GROUP, GROUP_END, SUBSCRIBE, SUBSCRIBE_UPDATE, SUBSCRIBE_OK,
SUBSCRIBE_STARTED, SUBSCRIBE_FINISH, SUBSCRIBE_SKIP, SUBSCRIBE_DROP, and
TRACKS_ADD change, and the Padding stream and datagram types go.

## Related

- [YIELDED deferred](/quest/m2/yield-defer.md) - removes continuations, the last use of GROUP's `Frame Start`
- [Refresh a track](/quest/m2/refresh-interval.md) - raises the `Wanted Group` this turns into a start time
