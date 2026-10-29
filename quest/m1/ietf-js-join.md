# [M] The JS moq-transport subscriber joins at the current group

## Goal

A late JS subscriber behind a spec-strict relay (such as moqx) receives the
group in progress whole, as the Rust subscriber does. A catalog or snapshot
written once arrives, and the first media group plays from its head. It works
on draft 18 and on draft 22.

## Plan

Decided 2026-10-28 (maintainer), from [#7088](https://github.com/moq-dev/moq/issues/7088):

- **Mirror Rust exactly.** On draft 18, SUBSCRIBE with Largest Object. After
  a SUBSCRIBE_OK with Largest `{G, k}`, send a Relative Joining FETCH
  (offset 0) as its own request, outside PUBLISH_DONE's Stream Count, and
  stitch the head and the live tail into one group producer. On draft 22,
  `relative(1)` carries the matching fill. Today it sends none, so the same
  gap exists there.
- **No join knob.** Every subscription joins at the current group. Rejected:
  `join: "current" | "next"` and @moq/watch joining media at the next group.
  [Fill refill](/quest/m2/ietf-fill-refill.md) already rejected the next group
  ("costs a GOP of startup"). Lite and Rust have no such anchor, and how far
  behind live to play is the player's choice through `maxDelay`. A "next"
  anchor, if ever wanted, belongs in
  [JS ranges](/quest/m1/subscribe-ranges/js.md) shared with Rust and lite.
- **No standalone-FETCH fallback.** A relay that refuses the joining FETCH
  for a lone subscriber (moqx: "Session has no active subscribe", reported as
  [openmoq/moqx#883](https://github.com/openmoq/moqx/issues/883)) has a bug of
  its own. A refusal costs only that group's head, as in Rust
  ([#6407](https://github.com/moq-dev/moq/pull/6407)).
- **No FETCH_OK deadline**, matching Rust: the joining FETCH lives as long as
  the subscription. FILL_TIMEOUT is a separate publisher-side bound.
- **Rust's strictness on duplicates.** A whole group from object 0 that
  arrives before the head wins and the FETCH is discarded. One that arrives
  after the head is refused.
- **Implementation:** take over [#7089](https://github.com/moq-dev/moq/pull/7089)
  (michaelstuurman, maintainer edits allowed). Trim it on its branch: drop
  `Track.Join`, `Subscription.join` and the @moq/watch `MEDIA_JOIN` change,
  drop the standalone fallback and the 10 s per-request deadline, and add the
  draft-22 fill. The author keeps credit, and the review history stays with
  the PR.

Update the notes this makes stale: [IETF fill timeout](/quest/m1/ietf-fill-timeout.md)'s
"JS sends no IETF FETCH", and the interop README's claim that a JS subscriber
requests neither a joining FETCH nor a fill.

Tests: the unit tests in #7089 minus the knob, plus a draft-22 fill. The
interop late-join lane passes with a JS subscriber against a strict publisher
(a lane that refuses to replay the group). `just ci` runs green, since #7089
has not yet run CI.

Public API: none. Wire: the JS subscriber sends Largest Object and a joining
FETCH on draft 18, and a fill on draft 22.

## Closes

- [#7088](https://github.com/moq-dev/moq/issues/7088) - a late draft-18 JS subscriber never receives a catalog written once

## Related

- [JS publisher answers a late join](/quest/m2/ietf-js-joining-fetch.md) - the publisher half after a quick finish
- [Headless moq-transport groups](/quest/m2/ietf-headless-group.md) - what a subscriber does with a tail whose head never comes
