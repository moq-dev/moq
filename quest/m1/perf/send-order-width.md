# [L] Widen the transport send order so groups stop needing a queue

## Goal

`web_transport_trait::SendStream::set_priority` takes a `u8`, and that single
signature is why the Lite publisher carries a `PriorityQueue` at all. The queue
exists to compress an unbounded total order over live group streams,
`(track desc, subscribe asc, group desc)`, into 256 dense ranks, which costs a
session-wide lock, a vec shift, and a wake per reordered group.

Widen the send order and the queue goes away on the backends that can carry it:
each group packs its own rank from values it already holds. Ordering between
subscriptions at the same track priority becomes undefined, deliberately, and
fairness moves to the transport scheduler where round-robin is expressible.

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

An exact order-preserving pack of the full key needs 136 bits, but the key does
not have to survive intact. Only two of its three terms are worth carrying.

`group` maps ascending to ascending. Newest first means a higher sequence wants
a higher send order, so it is the identity and only has to fit. Truncate it,
`group & (WIDTH - 1)`, with no per-subscription state at all. Do not rebase to
the subscription's first sequence: that looks tidier but collapses every group
backfilled below the base onto one ordinal, losing their order against each
other, while truncation ranks any two groups within WIDTH correctly whichever
side of the start they fall on. Truncate the sequence itself, and do not
substitute a counter bumped at group open: open order and sequence order diverge
whenever groups arrive reordered at a relay, when a `Group Start` backfills, and
on a FETCH range, and `Priority` orders on the sequence.

The cost is the wrap, when a subscription's live window straddles a boundary and
the newest group truncates low, briefly sorting under the backlog it should
preempt. At the widths below it is centuries away, since `append_group` advances
by one.

`subscribe` is dropped, and same-priority ordering becomes explicitly undefined.
A flat total order cannot express round-robin: whatever occupies that field
picks a winner, and the winner then takes strict precedence. The current
tie-break only reads as fair because each subscription usually has one live
group, so they alternate as groups complete; under congestion the favoured
subscription's backlog starves the other outright. Any packed variant inherits
that, and a group-sequence tie-break inherits worse, because cadence decides it:
audio at a 20ms cadence outruns video's sequence numbers by an order of
magnitude regardless of which matters more. Promising fairness here and
delivering precedence is worse than declaring it undefined.

That leaves `[track: 8][group: rest]`.

Fairness between equal-priority subscriptions belongs in the transport
scheduler, which is the only layer that can round-robin. The W3C primitive is
[`sendGroup`](https://www.w3.org/TR/webtransport/#sendGroup): streams sharing a
send group are strictly ordered by `sendOrder`, and bandwidth is allocated
equally between send groups. One send group per subscription with
`sendOrder = [track][group]` is the shape we want, with one catch worth knowing
before designing around it: send groups carry no priority relative to each
other, so a send group per subscription also flattens track priority across
subscriptions. `sendGroup` is the tool for the equal-priority case, not a
replacement for `sendOrder`.

Natively there is no equivalent. quinn schedules on a single `i32`, so
round-robin there needs a fork or an upstream change. quiche's `incremental`
flag round-robins within one urgency level, which is the same idea with one bit
instead of a group id, so it cannot both round-robin between subscriptions and
keep newest-first inside one.

Bit budgets, which are looser than the field types suggest:

- quinn, 32 bits. The full `i32` range is usable, negatives included, as long as
  the pack preserves order: build the key unsigned and store
  `(key ^ 0x8000_0000) as i32`, so unsigned order maps onto signed order. That
  gives `[8][24]`, and 24 bits is 16.7M groups, about 194 days at a one-second
  GoP. `i32` is quinn's own API rather than something the trait imposes, so
  widening `web-transport-trait` does not lift it, but at this layout it does
  not need lifting.
- Browsers, about 54 bits. `sendOrder` is IDL `long long`, but WebIDL converts
  to it through `ToNumber`, so the value crosses as a double and only integers
  within +/-(2^53 - 1) survive exactly. Past that, distinct orders round
  together into a silent tie rather than an error, and because rounding drops
  the low bits it is `group` that loses resolution first. A BigInt is not a way
  out: `ToNumber` on one throws. web-transport-wasm's own `set_priority` takes
  an `i32` today, so it needs widening too, just not as far as the IDL suggests.
- quiche and qmux, 8 bits. `track` alone fills the alphabet, leaving nothing for
  `group`, so these keep a queue or accept `incremental` round-robin in place of
  newest-first. Decide that before assuming they can drop the queue; the
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
