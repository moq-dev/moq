# [L] Lite-07 FETCH is single-group only

## Goal

On lite-07, FETCH reads one group or a slice within one group, and SUBSCRIBE
reads ranges of whole groups and never slices within a group. Rust and JS read
any multi-group range through SUBSCRIBE and use FETCH only for a single group
or a partial one, and the draft says so. Published versions (lite-03..06) keep
FETCH as it is.

## Plan

Decided 2026-11-03 (maintainer, on
[moq-dev/moq#7675](https://github.com/moq-dev/moq/pull/7675)), replacing the
2026-10-25 plan to remove FETCH from lite-07: "we can keep FETCH then but for a
single group (or slice within a single group). SUBSCRIBE is for multiple groups
and does not support slicing within a group." Route recovery
(`resume::Recover`), which fills from frame N, keeps its FETCH
(`group::Fetch::frame_start`).

Decided 2026-10-25 (maintainer, on
[moq-dev/moq#6807](https://github.com/moq-dev/moq/pull/6807)) and still
standing: Rust and JS change together, since both request and serve wire FETCH
on lite-07 (`track.Consumer.fetchGroup` in JS).

- Lite-07 SUBSCRIBE and SUBSCRIBE_UPDATE anchors stop carrying a frame: an
  Absolute anchor names a group, so a Range covers whole groups. This is a wire
  change to `Range`, the Positions section, and the changelog bullets that say
  SUBSCRIBE may start at any frame. A read starting partway through a group goes
  through FETCH instead.
- Rust and JS send FETCH on lite-07 only for one group or a slice of one
  group; a fill of more than one group goes through the subscription, as the
  Rust copy does since
  [moq-dev/moq#7872](https://github.com/moq-dev/moq/pull/7872).
- Decided 2026-11-04 (maintainer, on
  [moq-dev/moq#7872](https://github.com/moq-dev/moq/pull/7872)): the JS port
  of those subscription fills is this quest's, not #7872's. JS copies started
  filling only in [moq-dev/moq#7845](https://github.com/moq-dev/moq/pull/7845),
  after #7872 was scoped as the Rust copy's fill, and they FETCH each missing
  group one at a time, which breaks no wire rule. Port all of #7872's Rust fill
  rules to the JS subscriber (`Subscriber.#runFills`), with tests:
  - Subscription ranges: a lite-07 miss of more than one whole group joins the
    subscription's ranges beside the demand's, told apart by Range ID, or rides
    a fills-only SUBSCRIBE when no demand is open.
  - Recovery: a group the subscription drops as stale, or delivered before and
    since evicted, is asked once on a SUBSCRIBE of its own with no max delay.
  - Skips: a range start past what a fill asked refuses only the fills owing
    those groups now, while that range is live, and records nothing.
- `drafts/draft-lcurley-moq-lite.md` states the split in its Fetch and
  Subscribe sections. Run `just drafts check`.
- Run `just test interop --all`.

Public API: none expected. Wire: lite-07 (unpublished) SUBSCRIBE anchors lose
their frame.
