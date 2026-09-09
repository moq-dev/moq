# [L] Ladder controller

## Goal

One controller owns every generated rung's share, encoder target, stalled
state, and send order for one output bandwidth domain, so a ladder adapts to
its uplink instead of encoding every live rung at its ceiling, and transport
shedding drops the top of the ladder first.

## Plan

Add an optional bandwidth input to the transcode configuration and wire the
CLI's publisher session into it. `transcode` is not stageable
(`rs/moq-cli/src/args.rs:593-596`), so it dials its own session
(`rs/moq-cli/src/main.rs:236`, `rs/moq-cli/src/transcode.rs:130-140`) and
that session is the whole bandwidth domain. Supplying no input preserves
today's fixed-rate behavior exactly and never publishes congestion-induced
`stalled` state, which is what keeps this additive.

The controller subdivides that estimate across the ladder and applies the
band boundary from the [questline](/quest/m2/ladder/README.md), including the
lowest rung's `max / 3` case. `moq_transcode::Ladder` is ascending: `new`
sorts by configured maximum (`rs/moq-transcode/src/ladder.rs:101`) and
`rungs()` is lowest first (`:125-128`), so the next lower rendition the
formula reads is the preceding entry and the lowest rung is the one with
none.

### One priority for allocation and send order

Assign descending `track::Info::priority` down the ladder. Every rung is
stamped `PRIORITY.video` today (`rs/moq-transcode/src/rung.rs:133`,
`rs/moq-transcode/src/lib.rs:352`, `:409`). The allocator fills a tier
before the next sees a bit, so that alone protects lower rungs' allocation.

Honor the same number in `Priority::cmp`
(`rs/moq-net/src/lite/priority.rs:48-62`): subscriber priority stays first
(`:51-53`), then the publisher's `track::Info::priority` breaks the tie,
then the subscribe-id fallback (`:56-59`), then newest group. That is what
`Info::priority` already claims to do (`rs/moq-net/src/model/track.rs:100-102`),
and it is what the `BitrateUnsupported` fallback leans on: without it, an
encoder that cannot retune degrades the whole ladder equally instead of
protecting the bottom. A subscriber asking for a higher rendition ahead of a
lower one still gets what it asked for. Fix the allocator doc that presents
send order as unrelated (`rs/moq-net/src/model/bandwidth.rs:229-238`).

### Keep honest

- **Requested and applied targets are different numbers.** Catalog state
  follows what the encoder accepted. A transient rate-control failure keeps
  the last applied target and retries on a later material movement.
- **`BitrateUnsupported` is an explicit fallback, not a silent one.** Mirror
  moq-video's handling (`rs/moq-video/src/encode/producer.rs:383-386`): such
  an encoder keeps its configured maximum, publishes `stalled: true` whenever
  the allocation is below it, and clears only when the full maximum fits
  again. It reclaims no encoder work by design, and it must be visible in
  logs and tests rather than pretending the target was applied.
- **An idle rung must be able to recover.** Only demanded rungs consume
  allocation, but a rung that loses all demand while stalled cannot be left
  permanently stalled. Evaluate its hypothetical share against the current
  estimate and active lower-priority reservations, without giving it a real
  share or encoding probe traffic.

Acceptance: one demanded rung reaching its configured maximum on a permissive
uplink; several rungs sharing one uplink with lower ones protected; a
supported rung adapting down, clamping, stalling, and recovering without its
advertised maximum moving; the 5 Mbps over 2.5 Mbps boundary landing near
3.33 Mbps; the default 350 kbps lowest rung stalling near 117 kbps; an
unsupported encoder holding its maximum and not recovering early; no
bandwidth input preserving existing behavior exactly; and for send order,
equal subscriber priority where the lower rung wins, conflicting subscriber
priority where the subscriber's order wins, and a custom ladder order.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - builds on dev-only code that reaches `main` with the merge

## Related

- [Hierarchical stream scheduling](/quest/m2/quic/scheduler.md) - supplies the
  fair subscription buckets beneath this rendition policy
