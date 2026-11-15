# [M] The lite-07 wire addresses groups by start

## Goal

moq-lite-07 carries no group sequence numbers: a GROUP names its start and
its predecessor's start, GROUP_END says where a whole group ends, and every
subscription bound and drop is a time. Datagrams keep a sequence of their own on their own track kind. Rust speaks it, the lite
draft specifies it, and lite-01..06 are unchanged.

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
  lite-07; see [the model](/quest/m2/time-addressing/model.md)). FRAME and
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
[the model](/quest/m2/time-addressing/model.md) makes the start. Folded in
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
an open model finding (`StreamWins.cfg`, owned by
[finished across updates](/quest/m3/lite-finished-across-updates.md)). The
case it serves is a relay reader whose budget widens after the upstream
dropped a group as OLD. Since this quest rewrites SUBSCRIBE_DROP,
SUBSCRIBE_UPDATE, and SUBSCRIBE_SKIP anyway, it can make OLD final on its
subscription: the widening reader asks with a SUBSCRIBE of its own, as the
draft already requires for delivered or finally dropped groups, and the
`Update` numbers, re-offers, reopen rules, and their model configs go, while
YIELDED continuations keep their hold. Also check whether GROUP's
previous-start link makes SUBSCRIBE_SKIP redundant.

Decided 2026-11-14 (maintainer, pick on [moq-dev/moq#8764](https://github.com/moq-dev/moq/pull/8764)): ✅ (a) OLD drops are final on their subscription: this rewrite deletes `Update`, the reopen and re-offer paths, and their model configs (rejected: (b) keep reopening and finish [finished across updates](/quest/m3/lite-finished-across-updates.md)).

Public API: none beyond [the model](/quest/m2/time-addressing/model.md).
Wire: lite-07-wip GROUP, GROUP_END, SUBSCRIBE, SUBSCRIBE_UPDATE,
SUBSCRIBE_OK, SUBSCRIBE_STARTED, SUBSCRIBE_FINISH, SUBSCRIBE_SKIP, and
SUBSCRIBE_DROP change.

## Required

- [Rust groups by start](/quest/m2/time-addressing/model.md) - the model this wire carries
