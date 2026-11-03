# [S] Small per-request and per-frame waste in the lite subscriber and js/net

## Goal

Four small hot-path costs are removed or measured, each with a before/after count:

- The Rust lite subscriber keeps one copy of a track name per subscription instead of copying it twice.
- `@moq/net`'s `Subscriber.#drift` stops walking the whole retained timeline on every read when a track has no info, or the quest shows no real subscriber reaches that state.
- The JS IETF and lite publishers encode an object or frame header into one scratch buffer instead of allocating per varint.
- The JS fetch paths' per-frame write cost is measured, and fixed like the live path if it's the same waste.

## Plan

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2, in the perf epic, one quest because each part is small and found by reading the same profiles.

- **Lite subscriber track name** ([moq-dev/moq#7500](https://github.com/moq-dev/moq/pull/7500)): #7500 brought the lite publisher's request serve to one copy per request; its allocation test's backtraces showed the subscriber also copying the name twice per subscription, in `SubscriberDriver::poll` and `TrackServe::prepare_establish` (`rs/moq-net/src/lite/subscriber.rs`). Count it the way #7500's `a_lite_request_keeps_one_copy_of_its_track_name` does, on the subscriber's side.
- **`Subscriber.#drift`** ([moq-dev/moq#7523](https://github.com/moq-dev/moq/pull/7523)): `js/net/src/track.ts` walks back from the cap for the newest stamped group, and when `info` is undefined and no group carries a timestamp, the loop never breaks. In #7523's first fixture (an un-accepted, unstamped `Track.Producer`) it took 74% of steady-state CPU at 1024 groups. First check whether a real subscriber can read with history while its track's info is still undefined (for example before the info arrives, or on a version or path that never sets it), and how many groups it holds then. If it can, remember where the last walk stopped or skip the walk until a stamped group arrives; if not, record why and drop this part.
- **Header encoding** ([moq-dev/moq#7526](https://github.com/moq-dev/moq/pull/7526)): `encodeVarint` (`js/net/src/stream.ts`) allocates an `ArrayBuffer(9)` and a view per varint, and an IETF `Frame.encode` makes five varints plus two `concat`s per object, about 12% self time in #7526's ietf 128x8 case and part of the GC share that grows with streams. Lite's `frameHeader` (`js/net/src/lite/publisher.ts`) does the same. Encode into one reused scratch buffer and copy out once. Measure with `just js serve-bench` in Chromium, as #7526 did.
- **Fetch paths**: lite's `#runFetchGroup` and the IETF fetch writer still write each frame separately with no give-up watch; #7526 didn't measure them. Measure first, then apply #7526's live-path treatment if the cost is the same.

Public API: none. Wire: none.

## Related

- [Performance audit](/quest/m2/audit-perf.md) - finds the next round of these
