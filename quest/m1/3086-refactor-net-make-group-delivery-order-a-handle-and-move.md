# [M] refactor(net): make group delivery order a handle, and move timestamp-based skipping into moq-net

## Goal

Implement and verify the behavior tracked in [#3086](https://github.com/moq-dev/moq/issues/3086)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming: the ordered-handle half landed on dev
as #3099. What remains: move timestamp-based group skipping from moq-mux
(poll_min/max_timestamp) into moq-net, settling the three open design
questions below.

### Issue context

`track::Subscriber` exposes two delivery orders as two methods on one handle:

- `recv_group` / `read_frame` walk an arrival index (`PlainSubscriber::index`)
- `next_group` seeks by sequence (`PlainSubscriber::next_sequence`)

The cursors are not independent. `recv_datagram` bumps `next_sequence`, and `read_frame` bumps both `index` and `next_sequence`. Interleaving the two orders on one subscriber is expressible today and the result is hard to reason about, so the rule lives in doc comments rather than in the type.

Proposal: make the order a handle rather than a method choice, e.g. `track::Consumer::ordered()` returning a cursor that only yields groups in sequence order, with the arrival-order cursor keeping the plain path. That makes mixing the two unrepresentable instead of merely discouraged, per the Public API Scrutiny rules in `CLAUDE.md`.

##### Timestamp-based group skipping

The longer-term goal is to move the group-skipping logic out of `moq-mux` and into `moq-net`. Today it lives in `rs/moq-mux/src/container/consumer.rs` (`poll_read`), which decides whether to abandon a stalled group by comparing `poll_min_timestamp` / `poll_max_timestamp` across buffered groups against a latency budget.

The reason it lives in `moq-mux` is that those timestamps come from parsing the container format through `F: Format`. That constraint has weakened: on Lite05+ the wire carries per-frame timestamps at the track's own timescale (TRACK\_INFO plus zigzag-delta frame timestamps), so `moq-net` can read them without knowing anything about the media. That keeps the "the relay does not know anything about media" rule intact.

Open questions before this is designed:

- What happens on wires that cannot carry the timescale (pre-Lite05 moq-lite, IETF moq-transport), where timestamps fall back to local monotonic milliseconds? A skip policy keyed on those means something different than one keyed on author timestamps.
- Does the skip policy belong on the ordered cursor itself, on `Subscription` (so the publisher's aggregate sees it), or both?
- What does `moq-mux` keep? Discontinuity marking and the empty-group boundary handling in `poll_read` are container concerns even if the skip decision moves down.

##### Context

Split out of the `poll_next_in_range` fix, which made the sequence-ordered path a `BTreeMap` seek instead of a full cache scan. That removed the performance argument for treating sequence order as the slower opt-in path; the API argument above stands on its own.

---

## Closes

- [#3086](https://github.com/moq-dev/moq/issues/3086) - close this issue when the quest finishes
