# [S] A live join with no lag limit says so instead of a huge Min Age

## Goal

A lite-07 Live range that reaches back past every group, such as the old
"oldest group plus max X" a lite-05/06 peer asks for, carries no lag limit
as a typed value: Min Age is encoded plus one, `0` meaning no limit, as Max
Age already is. In Rust (`track::Start::Live { min: Option<Duration>, .. }`)
and `@moq/net` the lag is optional. `PAST_EVERY_GROUP` (2^53−1 ms) is gone,
and nothing compares against it.

The wire changes only for `moq-lite-07-wip`. lite-05/06 and moq-transport
keep their encoding and map onto the typed value.

## Plan

Decided 2026-11-23 (maintainer, /quest-plan after
[moq-dev/moq#9409](https://github.com/moq-dev/moq/pull/9409) and
[moq-dev/moq#9444](https://github.com/moq-dev/moq/pull/9444) landed):

- Scope: ✅ type only Min Age (rejected: an `Until` End anchor ending a
  history range at a Live range's start, for fast catch-up; a separate Live
  vs Bounded tag per Range; rethinking Max Age against the subscription's own
  newest group; dropping the quest).
  - A history range that switches to a Max Age at whatever live edge the
    publisher places is an odd contract. Without that seam, every range keeps
    one regime for its whole life: history never expires, and a Live range
    expires by age behind the live edge.
  - A DVR seek is one history range with End None at 1x. "Go live" adds a
    Live range and retires the history one. A relay resumes with the Joined
    anchor.
- Wire shape: ✅ keep the anchors (rejected: an explicit Range Type tag).
  Live (0x3) and Joined (0x4) already mark a live range, and None/Time/Position
  a history one. History with End None stays legitimate: it follows the edge
  and never expires.
- Min Age None: ✅ encoded plus one, `0` = no lag limit, mirroring Max Age
  (rejected: a separate "live from oldest" anchor type). This follows
  AGENTS.md: use 0 or a typed field, never a huge value.
- A Live range may still carry a finite End (✅ keep, rejected: open-ended
  only).

Where the sentinel lives on `main`:

- Rust: `rs/moq-net/src/model/subscription.rs` (`PAST_EVERY_GROUP`,
  `Start::within`), and `rs/moq-net/src/model/track.rs`, which branches on
  `min >= PAST_EVERY_GROUP` when placing a join. That branch is the one that
  lets a copy answer from its cache without waiting for its upstream's live
  edge, so keep that behavior and key it on `None`.
- JS: `js/net/src/internal.ts`, `js/net/src/lite/subscribe.ts` (the pre-07
  mapping), `js/net/src/track.ts`, and `js/net/bench/drift.ts`.
- The pre-07 mappings: Rust `lite/subscriber.rs` `wire_span` and the
  publisher's legacy bounds, and JS `lite/subscribe.ts`. A pre-07 "oldest
  group + max X" becomes `Live { min: None, max: Some(X) }`.
- Draft: the Live (0x3) and Joined (0x4) anchors and the SUBSCRIBE_UPDATE
  retune list, plus a changelog line.

A breaking API change (`!`): `min` becomes optional in Rust, JS, and
`moq-ffi` if it exposes it. Migration: `Duration` → `Some(Duration)`.
