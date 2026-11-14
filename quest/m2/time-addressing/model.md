# [XL] Rust groups are addressed by their start

## Goal

In `rs/moq-net`, a group's identity is its start timestamp: the API and the
model carry no group sequence numbers, a reader seeks or starts from a time,
and no producer remembers group numbers. Lite-01..06 and moq-transport peers
keep working through a group ID equal to the start in the track's timescale,
and lite-07-wip carries that same value until
[the lite-07 wire](/quest/m2/time-addressing/lite07.md) replaces it.

## Plan

Decided 2026-10-25 (maintainer). Why: group numbers need timelines just to
map to and from time, and reusing a group number poisons caches, so resuming
a producer would need per-track memory of its last number.

- **A group's address is its key.** A group our producers write is keyed by
  its first frame's PTS, known when the group opens. A group ingested from a
  legacy wire is keyed by the wire's group ID, verbatim (decided 2026-11-03,
  below). Frames inside a group stay in decode order and their PTS may go
  backwards (B-frames).
- **Starts strictly increase.** An equal start is refused loudly like a
  rewind; `Neighbors::admit` (`model/track.rs`, about line 502) allows ties today.
- **Three ways a group ends**: `finish(end)` with an exclusive end timestamp
  (the group is whole), `close()` (incomplete, but every frame written
  arrives), and `abort(err)` (reset). A relay closes an expired or partial
  group instead of resetting it.
- **Reach** (decided 2026-10-26, maintainer): a finished group's own end
  bounds it, else its successor's start, so the last group of a paused or
  finished track ages out without a successor (`TrackState::reach`).
- **Every write takes a timestamp, and nothing is ever wall-clock stamped**
  (decided 2026-11-03, below). A track without a timescale is scale-less: its
  keys and frame timestamps are bare numbers in no known unit.
- **Gaps.** The model keeps each group's previous start, so a cache or reader
  sees a hole without a round trip. The first group of an epoch has none, and
  the gap before it is unknown. Restarts keep minting a new epoch with fresh
  PTS; replicas in one epoch agree by PTS without coordinating numbers.
- **Legacy egress.** The group ID on lite-01..06 and moq-transport is the
  start in the track's timescale, so every relay agrees without a table.
  Published `@moq/net` decodes a group ID with `u53` and refuses one past
  2^53 (`js/net/src/lite/group.ts`), so a track whose starts pass that, such
  as nanosecond epoch time, fails those readers.
- **Legacy ingest.** Every legacy wire's group ID is taken verbatim as the
  key, timed or not, so relays agree and legacy egress round-trips the exact
  ID (decided 2026-11-03, below). A group whose ID is at or before its
  predecessor's ends the track with an error.
- **Ranges.** `Absolute(group)` ends and sequence-range drops become times.
  `Relative(n)` stays but counts groups instead of subtracting from an ID.
- **Name reuse** landed in
  [moq-dev/moq#6780](https://github.com/moq-dev/moq/pull/6780); the cut
  removes its sequence half, leaving a re-created track to keep its PTS
  moving forward.
- **What it waits on** (decided 2026-10-27, maintainer; Fetch as a range
  finished in [moq-dev/moq#7625](https://github.com/moq-dev/moq/pull/7625)): only the
  subscribe-ranges children that put sequence ranges into the model and the
  wire, Lite-07 ranges ([moq-dev/moq#7872](https://github.com/moq-dev/moq/pull/7872),
  merged 2026-11-05) and
  Delete the old start bookkeeping (finished in [moq-dev/moq#8613](https://github.com/moq-dev/moq/pull/8613)),
  not the whole epic. Drop set ([moq-dev/moq#8476](https://github.com/moq-dev/moq/pull/8476)) lands first too: its
  runs keyed by group sequence convert mechanically to starts.
- **Old relays** (decided 2026-10-26, maintainer): a released relay fills a
  hole over lite-05/06 with one FETCH per integer ID, so sparse IDs cost it
  one FETCH per tick in the hole. Accepted: "This was a lingering problem.
  The entire point of (multiple) ranges is to avoid this issue."

- **Skips are declared, never inferred** (decided 2026-11-03, maintainer,
  /quest-complete, replacing that day's "eager skips";
  [moq-dev/moq#7670](https://github.com/moq-dev/moq/pull/7670)). A gap tells
  moq-net nothing, since groups may be created out of order. SKIP comes only
  from the producer: an explicit `skip_groups`, or `append_group`, which
  means "the next group" and so skips every key below it that never arrived
  and was not given up, since the producer's last append. `create_group`
  implies no skip. A skip is final: creating a group at a skipped key is
  refused. Today `append_group` leaves no gap of its own, so its skip only
  names holes earlier `create_group` calls left. `append_group(timestamp)`
  keeps the rule, and there it names the span between consecutive starts:
  every key between the previous group's start and the new one.

Every current role of the sequence needs a replacement or a reason to drop
it:

- The cache key, the duplicate check, the hand-out-once record
  (`delivered.rs`), and slot stamps.
- Successor, reach, expiry, the arrival ledger, tail accounting, fills and
  misses, and SUBSCRIBE_SKIP and SUBSCRIBE_DROP accounting. Successor and reach already scan the
  cache by range, so they tolerate sparse keys; `delivered.rs` runs, fills,
  misses, and `subscription/window.rs` treat `sequence + 1` as the next group
  and must follow previous-start links instead.
- Resume across route flips (`resume.rs`), mid-group `Position` and
  `frame_start`, priority order within a subscription, `Demand::wanted`, and
  refresh.
- Not datagrams: they are their own track kind with their own sequence.
- The legacy codecs: lite GROUP, SUBSCRIBE and SUBSCRIBE_UPDATE bounds,
  SUBSCRIBE_START and its largest, SUBSCRIBE_END, SUBSCRIBE_SKIP,
  SUBSCRIBE_DROP, FETCH, and
  every IETF Location, Filter, FETCH, Largest, and NEW_GROUP_REQUEST.

### Timestamps are required and legacy IDs pass through

Decided 2026-11-03 (maintainer, the `/quest-import` grill of
[moq-dev/moq#7573](https://github.com/moq-dev/moq/issues/7573)). Why: a
wall-clock or `now()` stamp on a write lands in a different epoch than the
broadcast's media. That is what gives #7573's C publishers an 8 s catalog
`delay`, and the convenience it buys has no use case. This replaces the
2026-10-27 audit's "producer arguments keep an optional timestamp", which
recorded only sequencing.

- **Required timestamps.** Every producer write in moq-net and every binding
  takes a timestamp. There is no `None`, and nothing is wall-clock stamped.
  Reads return a timestamp too.
- **Scale-less tracks.** A track with no timescale (moq-transport without
  TIMESCALE, lite-01..04, or an app with no clock) is scale-less. Its frame
  timestamp is its group's key, and the write API on a scale-less group takes
  no per-frame time. Max-age on it counts bare units, so `max-age=5` keeps
  groups within 5 IDs of the newest; the application knows how sparse its
  IDs are.
- **One unit per track.** A track's timescale is the unit of both its frame
  timestamps and its group keys. Legacy keys are assumed to be in that unit
  and nothing checks it, so time ranges compare keys numerically. A released
  publisher numbering groups 0, 1, 2 with ms frames gets ranges in ID units,
  which is accepted. Max-age keeps comparing frame timestamps, as today.
- **Legacy IDs pass through.** Ingest keys a group by the wire's group ID on
  every legacy wire, and egress writes the key back, so moq-transport group 6
  leaves as group 6 and reaches moq-lite as key 6. No group carries a sequence
  beside its key.
- **Broadcasts are all timed or all scale-less.** moq-mux's catalog producer
  stamps the catalog from the broadcast clock and refuses to list a track off
  that clock's timescale, or a scale-less track beside timed ones. moq-net
  tracks stay independent.
- **No local clock in the model.** Added 2026-11-07 (maintainer,
  /quest-plan from the API shape audit): `Timestamp::now()`, the only model
  API that reads the local clock, is removed, and its callers stamp from
  their own clock: moq-boy, moq-hls, moq-stats, moq-bench, hang's
  video example, moq-tokio's examples, and tests across moq-net, moq-json,
  moq-mux, and moq-hls. `Timed<P>` moves from moq-net's root to moq-json,
  since moq-net never uses it internally, and its `at` becomes required under
  the rule above. Every crate that names `moq_net::Timed` follows the move:
  moq-mux, moq-ffi, moq-c, moq-cli, moq-boy, moq-hls, moq-stats,
  and moq-net-tests.
- **One anchor rule.** Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): ✅ collapse `Anchor` to `{Oldest,
  Relative(n), Time(ts)}` with every end inclusive (rejected: keep
  `Absolute` and make it inclusive; only add `Oldest`). Today an `Absolute`
  end is exclusive while `Relative` and `Time` ends include their group
  (`rs/moq-net/src/model/subscription.rs`), and "oldest" is the sentinel
  `Relative(u64::MAX)`. Once keys are starts, `Absolute(key)` and `Time(ts)`
  name the same group, so `Absolute` goes; a legacy exclusive end `E` maps
  to `Time(E - 1)`, since legacy keys pass through. A legacy inclusive
  start `S` keeps its bound: it resolves to the first group keyed at or
  above `S`, never to an earlier group that covers `S`, and a regression
  covers sparse keys (4 and 7 with a start of 5 selects 7). An exclusive end of 0
  (today `sequence_bounds(..0)` and an empty range list's
  `Subscription::end()`) selects nothing, so it is dropped as an empty range
  before conversion, never saturated to `Time(0)`, which would include
  group 0. Regressions cover `E = 0` and `E = 1` beside the legacy round
  trips. `Oldest` is explicit and
  encoded the same on every wire (lite-07, lite-06 and earlier, IETF), where
  today the lite encoder treats any `back > MAX_QUIC` as oldest and the
  model does not. `Range::groups(RangeBounds<u64>)` and
  `Subscription::with_range` go. JS's omitted start keeps meaning `Oldest`.
  This absorbs the deleted anchor-oldest quest (from the 2026-11-07
  invariant audit, model-4).
- **Fence what grows after the cut.** Decided 2026-11-07 (maintainer, /quest-plan from the API sweep of open PRs and remaining quests): `Position` becomes
  `#[non_exhaustive]`, so byte-holes can add a byte offset without a break.
- **Wires without a start.** Check which moq-lite versions can encode a
  historical start; lite-01/02 may not. On any that can't, an `Oldest` or
  earlier start gets the live feed, and the PR documents that exception
  rather than changing the old format.
- **lite-05/06 egress.** Decided 2026-11-13 (maintainer, /quest-complete),
  below.

Decision prompts:

- Ingest key: ✅ ID verbatim; first PTS, renumbered; per wire.
- A sequence beside the key: ✅ no, one key; yes, both.
- Timescale meaning: ✅ one unit for keys and frames, no legacy flag; flag
  legacy keys.
- Enforcing timed broadcasts: ✅ the catalog producer; moq-net broadcast;
  convention only.
- A scale-less frame's timestamp: ✅ its group key; any increasing number.
- Max-age on a scale-less track: ✅ bare units; doesn't apply.

### The unscaled timestamp and the PR's open calls

Decided 2026-11-13 (maintainer, /quest-complete on
[moq-dev/moq#8638](https://github.com/moq-dev/moq/pull/8638)). Why: a
scale-less frame's timestamp is its group key (2026-11-03, above), so the
type that carries it should say so instead of leaving it absent. Asked why
JSON was special, the maintainer was told it isn't: only moq-json picks group
starts for the caller.

- **A scale-less key is a `Timestamp`.** ✅ `Timestamp { value: u64, scale:
  Option<Timescale> }`, with `Timestamp::unscaled(key)` for a scale-less
  track, whose value is the group key (a datagram's sequence). The scale-less
  case is a typed `None`, never a scale of 0, and `Timescale` stays
  `NonZero`. Every write takes a required `Timestamp`: moq-net's group and
  track `write_frame` (which now works on a scale-less track, at the key),
  `Frame::new`, datagram `append`, moq-json's `Timed::at`, and the JS, moq-ffi,
  moq-c, and binding counterparts, with names mirrored. A timestamp whose
  scale doesn't match its track, or an unscaled one naming another group's
  key, is `TimestampMismatch`. Rejected: a bare `u64` key with an
  always-scaled `Timestamp`; `Option<Timestamp>`.
- **Conversions that need a scale are fallible.** Picked in the PR:
  `convert`, `as_scale`, `as_secs`, `as_millis`, `as_micros`, and `as_nanos`
  return `Result<_, TimeOverflow>`, erring on an unscaled timestamp, and
  `Duration` is `TryFrom<Timestamp>`. There is no `Ord`: `PartialOrd` is
  temporal across scales, compares keys when both are unscaled, and is `None`
  between a scaled and an unscaled timestamp. `unscaled` takes any `u64`,
  since a wide legacy key passes 2^61. In JS, where keys are `bigint`, an
  unscaled key past 2^53 throws only when `value` is read. Confining
  conversions to a second, always-scaled type was the alternative; it adds a
  type to every binding for what one `Option` says.
- **`append_group(start)` stays.** A multi-frame group still opens before its
  first frame is written, so it needs its start up front.
- **moq-json `Timed::at`: ✅ required, with the unscaled key on a scale-less
  track.** A snapshot or header write opens its group at `at`, which must pass
  every group the track holds, and a delta, op, or stream frame inside the
  open group carries that group's key. Rejected: `Option<Timestamp>`;
  refusing scale-less JSON tracks.
- **lite-05/06 egress: ✅ as built.** The group ID is the key. A scale-less
  track declares milliseconds in TRACK_INFO, because those published drafts
  treat a `Timescale` of 0 as invalid, and its frames carry the group key.
  The code and the draft say this is forced by the published wire. Rejected:
  main's send-time stamps ([moq-dev/moq#5461](https://github.com/moq-dev/moq/pull/5461));
  refusing scale-less tracks on lite-05/06.
- **lite-01..04 lag by the hole budget: ✅ accepted.** Those wires can't say
  the starts between two groups were skipped, so an in-order reader waits its
  budget at each gap. Rejected: extending the legacy-ingest rule so keys below
  the newest arrival count as never arriving.
- **Released `@moq/hang` players reading new Legacy audio over lite-05/06: ✅
  accept the gaps.** The packages release independently, and old players
  report gaps until they upgrade. Rejected: shipping `@moq/hang` before or
  with the Rust publishers.
- **A resume after a pause over wires without declared skips: ✅ accepted.**
  It waits up to the playout delay and reports a gap. Rejected: a heuristic
  for older wires.

Taken by the agent under /quest-afk on 2026-11-13, for the maintainer's
review, from the PR's local reviews:

- **Own-end reach ships before the wire carries the end.** An origin whose
  producer calls `group::Producer::finish(end)` ages a paused track's last
  group out after its span. A relay receives the group with a bare FIN
  (`rs/moq-net/src/recv.rs` closes it), so it still bounds that group by its
  successor, as main does on every hop. Relays serve nothing main wouldn't, and
  the gap closes when lite-07's GROUP_END carries the end
  ([lite07.md](lite07.md)). Rejected: carrying the end on the wire in this PR,
  which is lite07's wire, draft, and JS change; holding own-end reach back
  until then, which reverts the 2026-10-26 decision above.
- **A transcode fill locates a gap through one source read.** A miss reads
  the source once, in key order, for the groups around it, and drops the span
  between them at once. A source holding no group at all can't place the
  read until it holds one, so a live rung waits for the source's next group,
  as any time-anchored reader of the source would, holding no encoder slot.
  A retired rung's drain drops the miss and refuses the rest of the fill
  instead, since nothing says the source lost it. Rejected: a fetch per
  microsecond, which costs millions of requests per GOP gap; dropping the rest
  when the read can't answer at once, which loses groups a remote or
  handler-served source has. The long-term answer is moq-net serving a fill
  from another track's range (or an at-or-after fetch), as a relay serves one
  upstream: new API in every language, so a follow-up rather than this PR.

### Spike evidence

The spike published groups at their start ticks (2 s GOPs at 1 kHz and at
90 kHz) over lite-03, lite-06, lite-07-wip, moq-transport-14, and
moq-transport-18, direct and through a relay, against a contiguous baseline
(prototype tests in
[moq-dev/moq@ebdfd55](https://github.com/moq-dev/moq/commit/ebdfd55fc606db8f2c990b937392b81291ab4715)).

- Unaffected on `main`: live delivery through `recv_group` and `ordered()`,
  history with `Range::oldest` and absolute ranges, max-age expiry, a fetch
  of a missing ID (prompt `NotFound`), and hole fills, which take a constant
  handful of FETCHes at any spacing since
  [moq-dev/moq#6724](https://github.com/moq-dev/moq/pull/6724) and
  [moq-dev/moq#6715](https://github.com/moq-dev/moq/pull/6715). Before them
  a relay opened one FETCH per ID (540,000 for one 90 kHz hole), which is
  what released relays still do. `@moq/net` matched on lite-06 and lite-07.
- Broken: `Relative(1)..` on lite-07, direct or relayed, resolves by ID
  arithmetic, delivering `[6000]` and then parking instead of
  `[4000, 6000]`.

Update `doc/concept`, the lite draft's legacy mapping, and the
Rust half of every consumer the API change breaks to compile;
[consumers](/quest/m2/time-addressing/consumers.md) and
[bindings](/quest/m2/time-addressing/bindings.md) own the behavior changes.
Test the legacy mapping both ways on every wire, with the
resume model (`rs/moq-net/src/model/resume_model.rs`,
`js/net/src/resume.model.test.ts`) as the oracle for resume.
The PR's Impact section says how to migrate.

Public API: breaking. `group::Info`, `create_group`, `append_group`,
`Position`, `Range` ends, `fetch_group`, `Subscriber::floor`, and group consumers
address by start timestamp, every producer write takes a required
timestamp, untimed tracks become scale-less, `Timestamp::now` is gone, and
`Timed` is moq-json's, and `Anchor` is `{Oldest, Relative, Time}` with inclusive
ends.
Wire: no format change; legacy and lite-07-wip group IDs become start ticks.
