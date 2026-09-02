# [L] Widen the transport send order so groups stop needing a queue

## Goal

`web_transport_trait::SendStream::set_priority` takes a `u8`, and that single
signature is why the Lite publisher carries a `PriorityQueue` at all. The queue
exists to compress an unbounded total order over live group streams,
`(track desc, subscribe asc, group desc)`, into 256 dense ranks, which costs a
session-wide lock, a vec shift, and a wake per reordered group.

Widen the send order and most of that disappears for the backends that can
carry it: the rank becomes a value each group computes for itself.

## Plan

What each backend takes natively today:

- quinn: `i32`, narrowed by the trait and widened straight back.
- browser: `sendOrder`, `long long` in the W3C spec; `web-transport-wasm` takes
  an `i32`.
- quiche: `stream_priority(id, urgency: u8, incremental)`, lower urgency first.
  A real cap, not an artifact.
- qmux: `u8`, into its own bucketed scheduler.

So the split is quinn and browsers on one side, quiche and qmux on the other.
Both sides are defaults somewhere: quiche for moq-uring, quinn for moq-tokio.

An exact order-preserving pack of the full key needs 136 bits, so this only
works because exactness is only required among *live* streams. The asymmetry
between the two unbounded fields is direction, not size:

- `group` maps ascending to ascending. Newest first means a higher sequence
  wants a higher send order, so it is the identity and only has to fit.
  Truncate it, `group & (WIDTH - 1)`, with no per-subscription state at all.
  Do not rebase to the subscription's first sequence: that looks tidier but
  collapses every group backfilled below the base onto one ordinal, losing
  their order against each other, while truncation ranks any two groups within
  WIDTH correctly whichever side of the start they fall on.

  The cost is the wrap. When a subscription's live window straddles a boundary
  the newest group truncates low and briefly sorts under the backlog it should
  preempt, which is the one situation newest-first exists for. Taken
  deliberately: it is seconds of one track being mis-ordered, it clears as the
  window advances, and nothing downstream breaks. `append_group` advances by
  one, so on a 33-bit field it is unreachable; it only becomes routine if the
  field is squeezed or a producer picks sparse sequences.
- `subscribe` does not need to be ordered at all, only *present*. `Priority`
  already declares which subscription wins a tie arbitrary, requiring only that
  it is stable and independent of either side's direction. The field earns its
  place by sitting above `group` in the key, which is what stops group sequence
  from deciding between tracks.

  So do not rank it. A rank is what would force a live-subscription table,
  saturation handling, and a sizing argument; a mask, `subscribe & (K - 1)`, is
  stable and direction-independent with no state whatsoever. Subscription ids
  are sequential, so K consecutive subscriptions stay distinct and a collision
  needs one still open K ids later. A colliding pair falls back to comparing by
  group, so size K to make that rare, not impossible.

Truncate the sequence itself; do not substitute a counter bumped at group open.
Open order and sequence order diverge whenever groups arrive reordered at a
relay, when a `Group Start` backfills, and on a FETCH range, and `Priority`
orders on the sequence.

An `i64` layout of `[track: 8][subscribe: 12][group: 33]` makes the
per-group path arithmetic over values the group already carries: no lock, no
shift, no wake, and `GroupServe` loses its `poll_next` priority arm.

`track` is fixed at 8: it is a `u8` on the wire and every level is meaningful.
So the whole budget question is how to split what is left between the other two,
and that split is not free, because bits taken from the subscribe field lengthen
the `group` wrap period and vice versa.

Do not size the subscribe field from `initial_max_streams_bidi`, which is what
bounds live subscriptions (every control stream is bidi: `Stream::open` ->
`open_bi`, driven here by one `max_streams` knob shared with uni streams, 1024
in moq-tokio and 10,000 in moq-relay). That cap is the wrong input twice over:
uni streams are one per group and dominate the budget, so subscriptions sit far
below it, and a mask need not cover the population anyway, only keep collisions
rare among the handful live at once. A player holds single digits. Four bits is
likely enough and buys `group` a great deal; settle it against a real
subscription-count distribution.

Dropping the field entirely is the one option to rule out. It looks like it
merely leaves same-priority ordering undefined, but it does not: two such tracks
would then compare by raw group sequence, so a long-running track sitting at
sequence 50,000 beats one that started a minute ago at sequence 10, every time,
until its backlog drains. That is precedence, not a tie-break, and it is exactly
the failure the scoping exists to prevent.

That makes three tiers, not two:

- Browsers, 53 bits. `sendOrder` is IDL `long long`, but WebIDL converts to it
  through `ToNumber`, so the value crosses as a double and only integers below
  2^53 survive exactly. Past that, distinct orders round together into a silent
  tie rather than an error, and because rounding drops the low bits it is
  `group` that loses resolution first. A BigInt is not a way out: `ToNumber` on
  one throws. `[8][12][33]` fits under the limit and still puts the wrap
  centuries away. web-transport-wasm's own `set_priority` takes an `i32` today,
  so it needs widening too, just not as far as the IDL suggests.
- quinn, 31 bits, and `i32` is quinn's own API rather than something the trait
  imposes, so widening `web-transport-trait` does not lift it. 31 bits are
  usable rather than 32, and after `track` there are 23 to divide, so the mask
  and the wrap period trade directly: `[8][8][15]` wraps every 32k groups, about
  nine hours at a one-second GoP, while `[8][4][19]` buys six days at a 16-value
  mask. The small end is likely fine for a player and tighter for a relay mesh
  link; settle it against a real subscription-count distribution.
- quiche and qmux, 8 bits. Not enough for a packed key at all; see the
  round-robin question above.

Open question for the narrow backends. quiche's 256 urgency levels fit `track`
exactly, and `incremental: true` would round-robin within a track, which is what
`lite::priority`'s standing round-robin TODO wants. But it drops "newest group
first", which is what makes a congested track shed its backlog instead of its
live edge. Decide that before assuming quiche can drop the queue too; the
fallback is keeping the queue on the narrow backends only, at the cost of two
scheduling paths to hold consistent.

Entry cost is a breaking `web-transport-trait` release, rippling through
web-transport-quinn, web-transport-wasm, qmux, iroh, and moq-net's own
`transport::poll::SendStream`.

Acceptance: the wide backends open and close a group without touching a shared
lock, the existing send-order and ordering tests pass unmodified, and `just
bench BASE` shows no relay regression on the narrow path.

## Related

- [Priority set_track wakes](/quest/m1/perf/priority-set-track-wakes.md) - dead if the queue goes away
