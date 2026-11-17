# [L] lite-07 freezes without YIELDED continuations

## Goal

On lite-07 a Group Stream reset to make room for a higher-priority one is a
final drop of its group on that subscription, as on lite-06: the publisher
names the group in SUBSCRIBE_DROP, and a reader that still wants the rest
FETCHes it from the frame it reached. No group pauses, no stream continues
another, and a group has two streams on one subscription only as a
duplicate. The YIELDED (0x3C) code, paused groups, and continuations leave
the draft, `rs/moq-net`, and `js/net`, and come back in lite-08 with
[takeover](/quest/m3/group-takeover.md), designed as one feature. FRAME
still leads with `Payload Length`.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Defer YIELDED continuations to lite-08 with takeover (rejected: keep
  them but stop pausing a group on a reset that arrives without its code;
  keep as is). This reverses the 2026-11-04 pick that stream credit lands
  before the cut, and the 2026-11-12 pick on
  [moq-dev/moq#8545](https://github.com/moq-dev/moq/pull/8545) of YIELDED
  over "TOO_FAR_BEHIND plus a drop". What changed since: OLD drops became
  final on 2026-11-14, which made "ask again yourself" the rule for every
  other lost group; takeover, the other half, moved past the cut on
  2026-11-13; and the facts below.

Facts found by the audit:

- The named consumer is a lossless reader of several hundred tracks under a
  browser's stream cap, which is later work downstream. Nothing planned for
  0.22 reads that way.
- Browsers lose the signal the pause depends on. Chromium drops a reset that
  lands right after the stream's data and Firefox drops its code
  ([browser reset reports](/quest/m4/browser-reset-reports.md)), so the draft
  tells a subscriber to pause on any reset without a code (`#yield`), and
  only JS does: [moq-dev/moq#8673](https://github.com/moq-dev/moq/pull/8673)
  was "JS only, Rust unchanged". A drop on the Subscribe Stream needs no code
  from the reset.
- No relay link yielded until
  [moq-dev/moq#8843](https://github.com/moq-dev/moq/pull/8843) on
  2026-11-15, because every relay link negotiated object records
  (`GroupServe::yields`, `rs/moq-net/src/lite/publisher.rs`). Fixing that
  uncovered a panic in the paused group's writer and a protocol violation on
  continuations.
- A continuation restarts at the reset stream's own `Frame Start`, so it
  resends the frames the reader holds. That is what a FETCH from a frame
  index returns without the resend, and `resume::Recover`
  (`rs/moq-net/src/model/resume.rs`) already issues one. A FETCH reply rides
  a stream the subscriber opened, outside the incoming cap that caused the
  reset.
- Cost, by `grep -ci 'yield\|paus\|continu'`: 188 lines in
  `lite/publisher.rs` and 264 in `lite/subscriber.rs`, plus
  `tests/integration/stream_credit.rs` (857 lines), the `test/stream-credit`
  browser harness, `Continuation.cfg`, and three of the seven rules in
  `rs/moq-net/verify/subscribe/SubscribeAccounting.tla`. The rules for
  reopened OLD drops left with [time addressing](https://github.com/moq-dev/moq/pull/8908).

What goes:

- Draft: the Yield section, the YIELDED row (0x3C), GROUP's `Frame Start`
  (a continuation is its last sender on a subscription; lite-06 keeps its
  field), the paused clauses in SUBSCRIBE_DROP's accounting rule, the continuation exception to the
  duplicate-stream rule with its "held unread" and "MAY end the earlier
  stream" rules, the `ietf` extension's never-yields sentence, and both
  changelog lines. The reset rule reads as lite-06's: the publisher resets
  the lowest-priority Group Stream and drops the group.
- Rust: `StreamError::Yielded` and `has_yield` (`rs/moq-net/src/error.rs`,
  `lite/version.rs`); the publisher's `TrackRun.paused`, `paused_tasks`,
  `continuing`, `GroupServe.rewind`, `give_way`, `shed`, `pause`,
  `best_paused`, and `drop_given_up_paused`; the subscriber's `PausedGroup`,
  `YieldVerdict`, `TrackEntry.paused`, `yielding`, `yield_streams`, and
  `Duplicate.paused`. The priority scheduling that picks which stream to
  reset stays.
- JS: the same in `js/net/src/lite/{publisher,subscriber}.ts`, `open.ts`,
  and `error.ts`, including the paths that infer a pause from a reset without
  a code or from a later stream for the same group.
- `verify/subscribe`: [time addressing](https://github.com/moq-dev/moq/pull/8908) re-derived the
  model around final drops, expiry closes, and `Previous Start`; remove its
  YIELDED half (`Continuation.cfg`, the paused rules, and the continuation
  steps) and rerun the rest.

Guidance for the PR:

- Keep `test/stream-credit` and `stream_credit.rs`, rewritten to the new
  contract: more live tracks than the cap, the highest-priority groups get
  the streams, every group the reader misses is named in a drop, and a
  reader that FETCHes a dropped group gets it whole. Run it on lite-06 and
  lite-07 in Chromium and Firefox, where both now behave alike.
- Check that the ordered reader and `Recover` do FETCH a group dropped as
  TOO_FAR_BEHIND when the reader has no max age, and say in
  `doc/concept/moq-lite.md` what a lossless reader under a stream cap sees.
- Run `just test interop --all`.

Public API: `StreamError::Yielded` leaves Rust and `@moq/net`. Wire:
lite-07-wip loses reset code 0x3C and the continuation rules; lite-01..06
are unchanged.

## Related

- [Takeover](/quest/m3/group-takeover.md) - designs paused groups and continuations again, with takeover, for lite-08
- [Browser reset reports](/quest/m4/browser-reset-reports.md) - the browser bugs that made the pause depend on a lost signal
