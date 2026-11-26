# [L] Replace lite-07 SUBSCRIBE_SKIP and stall timers with Previous Delta

## Goal

On lite-07, every `GROUP` header carries `Previous Delta (i)` to prove gaps in the data layer, `SUBSCRIBE_SKIP` is removed completely, `GROUP_END` stays at stream end for whole-vs-closed and presentation `End`, and all five stall-guessing timers are removed from lite-07 (remaining isolated to pre-lite-07 and `moq-transport`). `/verify-subscribe` (`SubscribeAccounting.tla`) proves zero-timer settlement on the new wire.

## Plan

Decided 2026-11-24 (maintainer, /quest-plan): `Previous Delta (i)` on the lite-07 `GROUP` header (`0` unknown, `1` every key below is empty, `d + 2` names `Previous Start = Group Start - (d + 1)`), `SUBSCRIBE_SKIP` removed from lite-07, `GROUP_END` kept.

Decided 2026-11-25 (maintainer, on [moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626)):

- JS lite-07 drops the tail and track graces, as Rust did. The tail's excused-stream bound stays until browser WebTransport exposes `RESET_STREAM_AT`.
- A skip nothing can prove goes out as `SUBSCRIBE_DROP(NOT_FOUND)`.
- The FETCH `Stall` timer stays until FETCH is removed.
- A reset that may lose its GROUP header (no `RESET_STREAM_AT`) names the group in `SUBSCRIBE_DROP` with the reset's error: `poll::Session::reliable_reset` says which, the publisher holds a header only when it is true, and `PlainReset.cfg` checks it.
- Local and remote tracks must not behave differently: no local miss grace, local holes settle on predecessor proof with zero timers. Consistent with [A relayed reader reports a refused fill as a gap a direct reader never sees](/quest/m1/relayed-refused-fill-gap.md).

[moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626) carries all of it but the local miss grace. `S12` moved to [A moq-transport finish settles a declared end's tail](/quest/m3/ietf-finish-shared-tail.md).

The local miss grace (`TrackState::miss_grace`, `ledger::GRACE`) settles a hole below a group made with `create_group`, which says nothing about the starts below it, since producers create groups out of order (moq-transcode's `claim_group` for fetched past groups, moq-mux on a sought start, moq-archive fill answers). Removing it needs the producer to say what a creation proves, a public API choice:

- A: creating a group above the largest proves every start between empty. Simple, but moq-transcode's later claim of a past group lands on a start already proven empty.
- B (recommended): a creation says whether its predecessor is known, `append_group` keeping today's proof. A hole no producer proved stays owed until the producer creates, skips, or ends it. A cross-language API change (Rust, JS, `moq-ffi`).
- C: ask upstream or give up a hole at once. Not proof-based, so a late out-of-order group is lost.

Of the 23 tests that wait out a grace, 5 in `model/track.rs` encode the local miss grace and change with it (`a_hole_below_an_undeclared_start_is_asked_after_a_bounded_grace`, `a_refusal_below_the_start_is_held_for_a_bounded_grace`, `an_abort_past_the_grace_ends_a_pending_tail_cleanly`, `a_reported_end_survives_a_longer_grace`, `a_resumed_history_read_accounts_for_cached_and_refused_groups`). The other 18 (`ledger.rs`, `tail.rs`, `tests/integration/track_tail.rs`) cover the pre-lite-07 and `moq-transport` graces, which stay.

Asked 2026-11-26 (maintainer, /quest-run): whether to remove `Previous Delta`, since out-of-order creation undercuts it, and what it gains over `RESET_STREAM_AT` and `SUBSCRIBE_DROP`. Answered on the PR: it is the only proof of emptiness that travels with the data and caches as final, timed tracks need one at every group boundary, and the out-of-order problem is the producer's to declare whatever message carries it. Recommended: keep it, and take B in its own quest.

## Waiting

- Maintainer: keep `Previous Delta` or replace it, and pick A, B, or C for the local miss grace. Settled once both choices are in this quest's Plan.
