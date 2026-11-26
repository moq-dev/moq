# [L] Remove lite-07 stall timers: GROUP_END and SUBSCRIBE_SKIP name every empty start

## Goal

lite-07 settles every start without a timer: a Group Stream, its `GROUP_END` (whose `End` accounts for the starts inside the span), a `SUBSCRIBE_SKIP` for any empty start no `GROUP_END` covers, a `SUBSCRIBE_DROP`, or `SUBSCRIBE_END`. All five stall-guessing timers are removed from lite-07 (remaining isolated to pre-lite-07 and `moq-transport`). `/verify-subscribe` (`SubscribeAccounting.tla`) proves zero-timer settlement on this wire.

## Plan

Decided 2026-11-26 (maintainer, on [moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626)), overriding the 2026-11-24 `Previous Delta` plan below: "Remove prev group timestamp. Include the final group timestamp in the previous group instead. It's literally needed to learn segment durations anyway", and a control message covers discontinuities.

- No `Previous Delta` on the lite-07 `GROUP` header. The previous group's `GROUP_END` `End` proves the starts up to it empty.
- Starts no `GROUP_END` covers (a discontinuity, a first group, a group closed without finishing, a `GROUP_END` a reset may have cut) go out as `SUBSCRIBE_SKIP`, main's lite-07 message, kept as its own message rather than a `SUBSCRIBE_DROP` code: a drop's code is a stream error that ends readers, a skip is final cacheable emptiness that ends none, and AGENTS.md asks for a separate message over a special value.
- Every drop is final on the wire: no skip goes out over a drop (the audit's W1/W5 override machinery stays deleted), except the re-tell of a `GROUP_END` a reset may have cut or the subscriber reported lost, which may name a start dropped before its group finished and changes nothing for it.
- For local tracks, a producer declaring a group's end was to replace the 1 s local miss grace. See Waiting: it also governs local fill handlers and logical tracks.

Superseded 2026-11-24 (maintainer, /quest-plan): `Previous Delta (i)` on the lite-07 `GROUP` header, `SUBSCRIBE_SKIP` removed from lite-07, `GROUP_END` kept.

Decided 2026-11-25 (maintainer, on [moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626)), still in force:

- JS lite-07 drops the tail and track graces, as Rust did. The tail's excused-stream bound stays until browser WebTransport exposes `RESET_STREAM_AT`.
- The FETCH `Stall` timer stays until FETCH is removed.
- A reset that may lose its GROUP header (no `RESET_STREAM_AT`) names the group in `SUBSCRIBE_DROP` with the reset's error: `poll::Session::reliable_reset` says which, the publisher holds a header only when it is true, and `PlainReset.cfg` checks it.
- Local and remote tracks must not behave differently. Consistent with [A relayed reader reports a refused fill as a gap a direct reader never sees](/quest/m1/relayed-refused-fill-gap.md).

Decided 2026-11-26 (maintainer, /quest-run, on Codex review round 1 of [moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626)): fix in this PR the three cases only a grace settled, rather than keep a bounded lite-07 backstop grace or restore the graces until a follow-up.

- Starts outside every live range stop being owed when the range retires: a proven tail folds the ledger gaps no live range reaches (Rust and JS), and `finish_shared` ends a proven copy at its declared end, error included.
- The subscriber names a Group Stream it discarded before reading its end, such as one its transport evicted after the FIN, in a lite-07 SUBSCRIBE_UPDATE `Lost` list, and the publisher skips the starts inside its span again, as for a cut `GROUP_END`. A parked stream that fails before TRACK_INFO is accounted for at once, and the accept still opens and ends its group.

`S12` moved to [A moq-transport finish settles a declared end's tail](/quest/m3/ietf-finish-shared-tail.md).

## Waiting

- Maintainer: how to remove the local miss grace. `TrackState::miss_grace` (1 s) does more than settle a hole below an out-of-order `create_group`. It also decides when a local track with fill handlers (`Producer::requests`) asks for a hole, and how long a refused fill waits for the live feed before the hole is named dropped. Front and resume logical tracks are local producers too, so they would lose their gap give-up. Treating every local start as named broke 15 fill and resume tests.
  - A: a local track names every start. A local fill handler is asked at once, and its refusal is final at once. A hole with no handler stays owed until the producer creates, skips, or ends it. Logical and resume tracks follow their serving copy. Needs a group-level `finish_at` in `moq-ffi` (only `finish()` today) and JS parity, and about 15 tests change.
  - B: keep the grace only for fill-handler asks and refusals. Still a timer, so local and remote still differ.
  - **C (recommended):** keep the local miss grace in [moq-dev/moq#9626](https://github.com/moq-dev/moq/pull/9626), which settles the wire, and do A in its own quest. The wire change stands alone, and A is a public API change (`moq-ffi`, JS) with its own tests.
- Maintainer: what bounds lite-07's per-subscription accounting state, and what happens at the bound (Codex review rounds 2 and 3). Zero-timer settlement means each side keeps state until the wire accounts for it, with no grace to age it out:
  - The subscriber's tail ledger, which retiring a range cannot fold while another range awaits its SUBSCRIBE_START.
  - Retired gaps. Today they fold into the receipts for good, so a range that asks for one again reads it as accounted. On a subscription draining after SUBSCRIBE_END, the subscriber can then close before the group arrives.
  - The group keys of parked streams that failed before TRACK_INFO, before the copy's pool bills anything.
  - The subscriber's lost set, while its control stream is blocked.
  - The publisher's queued re-tells, which a merged range set bounds by the spans the cache holds at once, but not across cache turnover while the response is blocked.
  - Rust bills the ledger and the lost set to the copy's pool and folds the oldest gaps under pressure. Folding an owed gap is a synthetic receipt, so under pressure a subscriber can finish before a group arrives. JS sets no pool capacity.
  - The state is inherent: the publisher sends each group once per subscription, so the subscriber must remember what it was told until no range can ask for it again.
  - Options:
    - **A (recommended):** one per-subscription budget, billed to the pool on both sides and from SUBSCRIBE rather than from the copy's adoption. It covers the ledger, retired gaps kept apart from receipts, failed parked keys, the lost set, and the publisher's queued re-tells, the last checked against the groups the subscription sent. A subscription over budget ends with an error instead of folding an owed gap, and the relay subscribes again. An unstarted range with an absolute start owes only from that start, so fewer ranges block a fold.
    - B: fold as today under pool pressure, accepting a rare early finish, and bound JS by giving its pool a capacity.
    - C: never add a range over space the subscription retired; ask for it on a subscription of its own, as recoveries do. This removes the retired-gap case only.
- Maintainer: how to settle a lost GROUP_END once the publisher's cache evicted its group (Codex review rounds 2 and 3, finding 2).
  - `Lost` reaches the publisher about a round trip after the subscriber discarded the stream. If the cache evicted the group meanwhile, nothing says where its span ended, no skip goes out, and the starts only that GROUP_END covered stay owed. On a timestamp-keyed lite-07 track every group's span holds inner starts.
  - Options:
    - **A (recommended):** answer from the subscription's record. Tell a whole group's inner starts as a report kind of their own, so the record keeps each span's end after the cache evicts the group. The record is already per subscription and billed to the pool. A `Lost` naming a span the record folded away under pool pressure ends the subscription with an error.
    - B: end the subscription with an error whenever `Lost` names a group the cache no longer holds. Simplest, but a slow subscriber under cache pressure loses its subscription.
    - C: have the subscriber read a fully received stream's GROUP_END before the transport evicts it. moq-quic would have to hand moq-net the stream's tail instead of discarding it, and a browser's WebTransport cannot.
  - Either way, the TLA model gains publisher eviction and an acknowledgement before the application reads or discards the stream.
