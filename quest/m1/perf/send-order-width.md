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
  one, so on a 39-bit field it is unreachable; it only becomes routine if the
  field is squeezed or a producer picks sparse sequences.
- `subscribe` maps ascending to descending. The lowest id wants the highest send
  order, and inverting needs an upper bound the session does not have, since
  subscription ids grow forever. That one needs a rank among live subscriptions.
  The table is per-subscription rather than per-group, so it changes orders of
  magnitude less often than the queue does today.

Truncate the sequence itself; do not substitute a counter bumped at group open.
Open order and sequence order diverge whenever groups arrive reordered at a
relay, when a `Group Start` backfills, and on a FETCH range, and `Priority`
orders on the sequence.

An `i64` layout of `[track: 8][subscribe_rank: 16][group: 39]` makes the
per-group path arithmetic over values the group already carries: no lock, no
shift, no wake, and `GroupServe` loses its `poll_next` priority arm.

Width is a real parameter rather than a free choice, because it sets how often
that field wraps. Browsers have room to spare, but quinn caps at `i32`, so the
same layout there is nearer `[8][8][15]` and wraps every 32k groups, which at a
one-second GoP is every nine hours rather than never. Spend spare bits on
`group` instead of `subscribe_rank`; live subscriptions per session number in
the tens, so 8 bits is already generous.

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
