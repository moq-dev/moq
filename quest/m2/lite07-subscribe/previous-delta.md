# [L] Remove lite-07 stall timers: GROUP_END and SUBSCRIBE_SKIP name every empty start

## Goal

lite-07 settles every start without a timer: a Group Stream, its `GROUP_END` (whose `End` accounts for the starts inside the span), a `SUBSCRIBE_SKIP` for any empty start no `GROUP_END` covers, a `SUBSCRIBE_DROP`, or `SUBSCRIBE_END`. All five stall-guessing timers are removed from lite-07 (remaining isolated to pre-lite-07 and `moq-transport`). `/verify-subscribe` (`SubscribeAccounting.tla`) proves zero-timer settlement on this wire.

## Plan

Decided 2026-11-26 (maintainer, on [moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626)), overriding the 2026-11-24 `Previous Delta` plan below: "Remove prev group timestamp. Include the final group timestamp in the previous group instead. It's literally needed to learn segment durations anyway", and a control message covers discontinuities.

- No `Previous Delta` on the lite-07 `GROUP` header. The previous group's `GROUP_END` `End` proves the starts up to it empty.
- Starts no `GROUP_END` covers (a discontinuity, a first group, a group closed without finishing, a `GROUP_END` a reset may have cut) go out as `SUBSCRIBE_SKIP`, main's lite-07 message, kept as its own message rather than a `SUBSCRIBE_DROP` code: a drop's code is a stream error that ends readers, a skip is final cacheable emptiness that ends none, and AGENTS.md asks for a separate message over a special value.
- Every drop is final on the wire: no skip goes out over a drop (the audit's W1/W5 override machinery stays deleted), except the re-tell of a cut `GROUP_END`, which may name a start dropped before its group finished and changes nothing for it.
- For local tracks, a producer declaring a group's end was to replace the 1 s local miss grace. See Waiting: it also governs local fill handlers and logical tracks.

Superseded 2026-11-24 (maintainer, /quest-plan): `Previous Delta (i)` on the lite-07 `GROUP` header, `SUBSCRIBE_SKIP` removed from lite-07, `GROUP_END` kept.

Decided 2026-11-25 (maintainer, on [moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626)), still in force:

- JS lite-07 drops the tail and track graces, as Rust did. The tail's excused-stream bound stays until browser WebTransport exposes `RESET_STREAM_AT`.
- The FETCH `Stall` timer stays until FETCH is removed.
- A reset that may lose its GROUP header (no `RESET_STREAM_AT`) names the group in `SUBSCRIBE_DROP` with the reset's error: `poll::Session::reliable_reset` says which, the publisher holds a header only when it is true, and `PlainReset.cfg` checks it.
- Local and remote tracks must not behave differently. Consistent with [A relayed reader reports a refused fill as a gap a direct reader never sees](/quest/m1/relayed-refused-fill-gap.md).

`S12` moved to [A moq-transport finish settles a declared end's tail](/quest/m3/ietf-finish-shared-tail.md).

## Waiting

- Maintainer: how to remove the local miss grace. `TrackState::miss_grace` (1 s) does more than settle a hole below an out-of-order `create_group`. It also decides when a local track with fill handlers (`Producer::requests`) asks for a hole, and how long a refused fill waits for the live feed before the hole is named dropped. Front and resume logical tracks are local producers too, so they would lose their gap give-up. Treating every local start as named broke 15 fill and resume tests.
  - A: a local track names every start. A local fill handler is asked at once, and its refusal is final at once. A hole with no handler stays owed until the producer creates, skips, or ends it. Logical and resume tracks follow their serving copy. Needs a group-level `finish_at` in `moq-ffi` (only `finish()` today) and JS parity, and about 15 tests change.
  - B: keep the grace only for fill-handler asks and refusals. Still a timer, so local and remote still differ.
  - **C (recommended):** keep the local miss grace in [moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626), which settles the wire, and do A in its own quest. The wire change stands alone, and A is a public API change (`moq-ffi`, JS) with its own tests.
- Maintainer: how to close three cases that only a grace settled, which the zero-timer lite-07 now leaves open (Codex review round 1 on [moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626)).
  - A start no wire message names: the gap between two disjoint ranges of one subscription. A proven received copy waits on it forever, so a reader past it never reaches the track's end or error. Reproduced on the copy model: clearing the declared tail in `finish_shared` does not help, since a proven copy's hole wait never ends.
  - The subscriber's ledger (Rust `Tail::proven`, JS `Ledger.expire` with no grace) never folds a gap, so a subscription that keeps moving one range through disjoint parts of a track keeps a run per visited interval.
  - A Group Stream the subscriber's transport evicts after QUIC acknowledged its FIN, before moq-net read its GROUP_END, sends no STOP_SENDING, so the publisher never re-tells the cut span and the subscriber waits forever for those starts.
  - Options:
    - Fix all three in [moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626) (Recommended) - starts outside every live range stop being owed when the range retires (fixes the first two in Rust and JS), and the subscriber reads a fully received stream's GROUP_END before evicting it, or names it to the publisher. Grows an already L PR.
    - Keep a bounded lite-07 backstop grace for these cases only - smallest change, but brings a timer back against the 2026-11-25 decision.
    - Land the wire with the lite-07 graces restored, and remove them in a follow-up once all three are fixed - unblocks the lite-07 cut sooner, at the cost of moving the zero-timer goal out of this quest.
