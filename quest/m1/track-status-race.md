# [S] A subscribing reader never asks TRACK_STATUS

## Goal

A reader that subscribes to a track served over moq-transport reaches the
publisher as a SUBSCRIBE, never as a TRACK_STATUS, however its subscription
races the request it creates. TRACK_STATUS stays for fetch-only and
query-only demand, and a refused TRACK_STATUS fails that demand loudly.

## Plan

Decided 2026-10-19 by the maintainer (#5954):

- Fix the race at the source. `broadcast::Consumer::resolve_track`
  (`rs/moq-net/src/model/broadcast.rs`) makes the request visible before
  `Preferences::register` (`model/track/demand.rs`) writes the
  subscription, and the front's `Action::Query` (`model/origin.rs`) runs
  `lookup.query()` before `copy.subscribe`. Register the subscription
  before the request or demand edge is visible, and subscribe the front's
  copy with its query. Don't make the IETF subscriber wait or poll for a
  subscription.
- Delete the NOT_SUPPORTED fallback in `track_status`
  (`rs/moq-net/src/ietf/subscriber.rs`). It was added for the JS publisher,
  which answers TRACK_STATUS since #5468, and falling back to SUBSCRIBE for
  fetch-only demand brings back the problems TRACK_STATUS avoids (a
  finished track refuses it; it races the fetches). This settles the
  deferred step in [JavaScript FETCH](/quest/m1/js-fetch.md).
- Don't widen the fallback to undefined codes, as #5954 proposes: that would
  hide the peer's bug and ours.
- File an issue upstream for each relay found refusing TRACK_STATUS, or
  answering it with an undefined code: moqx is known (#5954, `0xFFFFFFFF`).
  The maintainer asked for these on 2026-10-19.
- Regression test: a subscriber's catalog read against a publisher that
  refuses every TRACK_STATUS with an undefined code still opens, and the
  publisher sees no TRACK_STATUS. It fails today on a multi-thread runtime.

Public API: none. Wire: none.

## Closes

- [#5954](https://github.com/moq-dev/moq/issues/5954) - a TRACK_STATUS refused with any code but NOT_SUPPORTED fails the reader's subscription
