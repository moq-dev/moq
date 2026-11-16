# [L] Media readers take declared ends from the group, and hang writes no markers

## Goal

A hang reader learns where a group, its last frame, and a declared stop end
from the group's own end: lite-07's GROUP_END carries the exclusive end
timestamp, and the Rust and JS container consumers turn it into the frame
end and `Read::End` that a marker produces today. hang then stops writing
the empty duration frames and discontinuity marker groups, and the hang
draft specifies neither, so moq-hang-04 never asks a publisher for them.
Every first-party consumer that numbered groups works by start timestamp,
in Rust and JS, and the HLS/DASH export places each group on the declared
join interval grid by its start.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ The whole change lands before the cut, readers and publishers both
  (rejected: only the reader half in m2, with the markers dropped in m3;
  keeping the 2026-11-14 plan and recording that 0.22 readers degrade).
  This reverses the 2026-11-14 move to m3, which rested on "what remains
  breaks nothing". The maintainer's reason: the point of GROUP_END was to
  carry the duration as a timestamp, and it does, so the markers have no
  job left.

What the audit found against "readers already tolerate their absence":

- The Rust reader produces `Read::End` only from a marker:
  `rs/moq-mux/src/container/consumer.rs` pops a marker into
  `Event::FrameEnd { end, opens }`, and `opens: true` becomes `Item::End`
  and then `Read::End`. The lite-07 group end is read in the same file and
  feeds only `presented_end`; it never becomes a `Read::End` or a
  per-frame end. JS is the same (`js/hang/src/container/consumer.ts`,
  `this.#format.end?.(sample)` and the `kind: "end"` emit).
- The producer writes both today: `rs/moq-mux/src/container/producer.rs`
  ends a group through `finish_group` (the marker) and the group's
  declared end, and `discontinuity()` writes marker groups.
- `Read::End` is matched in more than 20 Rust files outside the consumer
  (moq-audio decode, moq-c, moq-ffi, moq-gst, moq-cli play, moq-video) and
  in `js/watch`'s audio and video decoders. It is how a player learns of a
  declared stop: push to talk, declared silence, a drained encoder.
- So if the markers went after the cut, a published 0.22 reader against a
  newer publisher would stop reporting declared ends and lose the last
  frame's duration, and `Producer::discontinuity`, `Container::end`, and
  `Container::finish_group` would change meaning one release after they
  shipped. After the cut, 0.22 is the old player.

For the PR, in this order:

1. Settle what a reader can know without a marker group, first, with the
   maintainer if the answer costs API. A marker group tells a reader of a
   declared pause at once; a group's end alone does not distinguish "the
   publisher paused here" from "the next group follows", until the next
   group starts above that end or fails to arrive. Recommended: `Read::End`
   reports every group's declared end as it is read, a player treats media
   as ended at that time until a group starts, and a next group starting
   at the end is plain continuity. That gives a push-to-talk player the
   stop when the group ends, which is when the marker told it.
2. Readers: the Rust and JS container consumers derive the last frame's
   end, the group's end, and `Read::End` from the group's declared end and
   the next group's start. Add the test the audit asked for: a group that
   carries only a declared end, and no marker, yields the same reads as
   one with a marker.
3. Publishers: `container::Producer` stops writing the duration marker
   (`cut`, `close`, `finish_group`) and `discontinuity()` stops writing a
   marker group; it ends the open group at its declared end and the next
   group starts later. Remove or repurpose `Container::end` and
   `Container::finish_group`, and say in the PR what `discontinuity()`
   means now, or delete it if a declared end says it all. `js/publish`
   follows. Check audio's terminal-trim marker before deleting it: the
   draft has an empty payload give the source's exclusive end ahead of the
   codec's drain packets, so a decoder discards samples past it. A group
   end below its last packet's extent says the same; prove it with the
   Opus drain test, or keep that one marker and say why.
4. Readers stop accepting markers only if that deletes real code and no
   supported peer still sends them: a lite-05 or lite-06 session has no
   GROUP_END, so legacy ingest from an old publisher still carries
   markers. Keep reading them there; the 2026-10-25 rule below covers the
   other direction.
5. The hang draft's Container section: delete the duration and
   discontinuity marker rules, including the data-track marker
   ([moq-dev/moq#5815](https://github.com/moq-dev/moq/pull/5815)), and say
   that a group's end is the transport's.

Decided 2026-10-25 (maintainer), from the spike:

- GROUP_END replaces hang's empty-frame duration marker
  (`rs/moq-mux/src/container/producer.rs`, `cut` and `close`), so moq-mux
  stops writing empty frames.
- Discontinuity marker groups go away (`producer.rs`, `discontinuity`): a gap is a
  previous end below the next start, and a missing group is a
  previous-start link that doesn't match.
- Legacy egress synthesizes neither: a player on lite-05 or lite-06 loses
  end durations and pause markers. That is the best-effort service old
  versions get; it no longer describes a 0.22 reader, which reads the
  group's end.

Decided 2026-11-02 (maintainer): removing the HLS records, the per-rendition
`hls` field, and A/V segment alignment moved to m0 as the HLS grid,
landed in [moq-dev/moq#7652](https://github.com/moq-dev/moq/pull/7652), which also added
`media.join.interval`, moved `clock` to `media.clock` (both renamed by
[the sync section](/quest/m2/hang04/sync.md)), and settled the
segmentation rules decided here on 2026-10-27/28. It places groups by their
first frame's pts; this quest swaps that for the group start, without
changing the segment numbers.

Convert each consumer that numbers groups: hang and moq-mux timelines,
HLS/DASH export, `seek` for GOP-aligned failover in moq-mux, HLS import's
discontinuity packing, transcode's 1:1 numbering, archive keys and resume,
relay and CLI `fetch --group`, and player continuity checks in the Rust and
JS container consumers and `js/watch`. Update the hang draft and the docs
each one makes stale. Rename no CLI flag.
[The model](https://github.com/moq-dev/moq/blob/7d1bee79cb8fcb4d816fc53f0547ef8b2b19756a/quest/m2/time-addressing/model.md)
([moq-dev/moq#8638](https://github.com/moq-dev/moq/pull/8638)) already
converted the Rust consumers whose API named group numbers: `moq fetch
--group` and the relay's `/fetch?group=` keep their names and take a start,
moq-mux `seek` and reads carry starts, and moq-hls, moq-archive, and
moq-transcode key by start. `@moq/hang`'s `Read.group` keeps its name and
carries whatever `@moq/net` keys by.

Tests: the marker tests in `rs/moq-mux/src/container/producer.rs`
(`discontinuity_publishes_an_empty_group` and its neighbours) become tests
of declared ends; a push-to-talk stop reaches a Rust and a JS reader as an
end with no marker on the wire; a lite-06 session still reads an old
publisher's markers. Run `just test interop --all`.

Public API: breaking. Rust `moq_mux::container`: `Producer::discontinuity`,
`Container::end`, and `Container::finish_group` change or go, and
`Read::End` is reported from a group's declared end; `@moq/hang`'s container
consumer and producer mirror it. Wire: hang drops empty duration frames and
discontinuity marker groups on lite-07; the hang-04 draft never specifies
them.

## Required

- [@moq/net by start](/quest/m2/time-addressing/js.md) - the JS API these consume
- [Lite-07 by start](/quest/m2/time-addressing/lite07.md) - GROUP_END carries the end the duration marker did

## Related

- [Writing a group needs no invented start, and its end has one word](/quest/m2/group-api.md) - names the call that declares a group's end
- [The catalog's clock and join interval live under a sync section](/quest/m2/hang04/sync.md) - renames the interval the HLS grid reads
- [A group's frames present before the next group starts](/quest/m3/groups-never-overlap.md) - tightens the group rule a declared end relies on
