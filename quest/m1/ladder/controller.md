# [L] Ladder controller

## Goal

One controller owns every generated rung's share, encoder target, and stalled
state for one output bandwidth domain, so a ladder adapts to its uplink
instead of encoding every live rung at its ceiling.

## Plan

Add an optional bandwidth input to the transcode configuration and wire the
CLI's publisher session into it. Supplying none preserves today's fixed-rate
behavior exactly and never publishes congestion-induced `stalled` state, which
is what keeps this additive.

The controller subdivides that estimate across the ladder and applies the
band boundary from the [questline](/quest/m1/ladder/README.md), including the
lowest rung's `max / 3` case. Assign descending `track::Info::priority` down
the ladder: the allocator already fills a tier before the next sees a bit, so
that alone protects lower rungs' allocation without touching the scheduler.

Three things the implementation has to keep honest:

- **Requested and applied targets are different numbers.** Catalog state
  follows what the encoder accepted. A transient rate-control failure keeps
  the last applied target and retries on a later material movement.
- **`BitrateUnsupported` is an explicit fallback, not a silent one.** Such an
  encoder keeps its configured maximum, publishes `stalled: true` whenever the
  allocation is below it, and clears only when the full maximum fits again. It
  reclaims no encoder work by design, and it must be visible in logs and tests
  rather than pretending the target was applied.
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
unsupported encoder holding its maximum and not recovering early; and no
bandwidth input preserving existing behavior exactly.

## Required

- [Rung order](/quest/m1/ladder/order.md) - the band formula reads the next
  lower rendition's configured maximum
