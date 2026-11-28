# [M] A relayed reader reports a refused fill as a gap a direct reader never sees

## Goal

An ordered reader of a relay copy reports the same items as the same reader at the publisher,
in Rust and JS. Asking from a time before every group, both read the publisher's drop of the
range from one past the time, then the later group, whichever reader subscribes first.

## Plan

Kept by the maintainer on 2026-11-24 ([pick round](https://github.com/moq-dev/moq/pull/9529#issuecomment-7140096782)).

Decisions (maintainer, /quest-run, 2026-11-25):

- A local reader whose time range resolves through the history handler asks it for the keys
  with no group held, as copies and JS already do. Already true on `main`: a local track with a
  handler judges its holes misses at once (`TrackState::immediate_misses`).
- Once a history lookup comes back empty from key 0, the range starts one past the time, in Rust
  (`Window::found`) and JS (`Window.found`): the time's own key is out of range everywhere.
- A publisher answers a range with a start, never "no group to deliver", whenever it dropped
  keys in it, so a copy waits for those drops rather than placing the time before every group.

[moq-dev/moq#9830](https://github.com/moq-dev/moq/pull/9830) lands the Rust side:
`Window::found` starts past the time, and the lite publisher's `TrackRun::history_start` answers
a range otherwise left empty below delivery at the first key it reports
(`track::Subscriber::first_reported`), rescanning drops when that lowers the start.
`waiting_time_start::a_range_before_every_group_reads_past_it` and
`a_relayed_range_before_every_group_waits_for_its_drops` assert `[Err(11..21), Ok(66)]` for
both readers in both orders, with and without the front-drop fix's `find_gap` before every read.

What remains, JS only:

- `js/net/src/window.ts` `Window.found`: a `dropped` answer with floor 0 starts at `at + 1n`, and
  the mirror test in `js/net/src/fill.test.ts` of `a_time_start_before_the_cache_asks_nothing_below_it`
  follows the Rust one (asks `6..10`, reads no 5).
- `js/net/src/lite/publisher.ts`: the same `history_start` rule where it answers a range's
  SUBSCRIBE_START, if it answers ranges below delivery with no start as Rust did.
- A JS twin of the two integration orders, or an interop case, showing JS readers agree.
