# [XL] Scope and sign track priority

## Goal

Priority orders streams only within one scheduling domain: the streams a single
party owns both ends of, the first mile from publisher to ingest relay and the
last mile from edge relay to viewer. On a shared relay-to-relay session the
domain is the broadcast: when the relay enables it, broadcasts share the link
fairly and priority orders streams only inside each broadcast, so one
customer's viewers asking for 255 never starve another customer's broadcast.

In the same API break, every priority becomes an `i8`, higher first, with 0 as
the unset midpoint: `track::Info`, `Subscription`, and `group::Fetch` in Rust,
their JS counterparts, moq-ffi, moq-c, and every wrapper. Nobody has to know
that 127 is the middle of a `u8`. The wire stays a byte.

## Plan

Where the model stands today, all in `rs/moq-net`:

- One `PriorityQueue` per session, shared by the moq-lite and moq-transport
  publishers, a strict global sort over every in-flight group on the
  connection: a subscription at
  priority 200 pre-empts one at 100 indefinitely, and only the top 255 groups
  get a distinct rank.
- `Priority` already ranks a group by its track priority, then its subscription
  id, then group within that subscription in its order (`priority.rs`). JS
  packs track priority and the group's position within its own subscription
  into one send order (`js/net/src/priority.ts`), so two tracks at equal
  priority interleave rather than one draining first.
- Group order within a track follows each subscription's `order` (#5385),
  newest first by default.
- A relay forwards the max of its downstream subscriber priorities upstream
  (`model/subscription.rs`, `lite/subscriber.rs`, and on moq-transport
  upstreams `ietf/subscriber.rs` since #5922), never the publisher's track
  priority, so one viewer asking for 255 raises that track above every other
  tenant's on the cluster link (`rs/moq-relay/src/cluster.rs`: one
  bidirectional session per peer pair carries every broadcast). The draft
  already says the upstream leg SHOULD use the publisher priority.

Decided 2026-09-19: the domain on a relay-to-relay session is the broadcast,
keyed by its path, and it is a relay setting (`cluster.fair = true`), off by
default so a single-tenant deployment keeps strict priority across the link.
A tenant-keyed domain (one bucket per customer, however many broadcasts they
run) needs the tenant identity from the auth line first and is a later
refinement of the same tier, not a different design. Opening more
subscriptions must never buy a broadcast more than its bucket allows. Until
tenant keying lands, a peer that opens more broadcasts does get more shares,
on send and receive alike; tenant keying is what closes that.

Decided 2026-10-08: the signed priority type lands here, so the priority API
breaks once instead of twice. Subscribe ranges owns group order, so this quest
neither forbids an order knob nor edits the doc's Order row.

Direction to settle in the draft first, then the code:

- On the last mile the viewer owns the whole session, so its audio and video
  subscriptions share one domain and the wire is unchanged. On a relay-to-relay
  session the relay knows every subscription's broadcast, so no carrier is
  needed on the wire; the draft only has to say that a relay MAY scope
  priority to the broadcast.
- On a relay-to-relay session with fairness on, ordering across broadcasts is
  byte-fair (the scheduler's fair tier, one domain per broadcast and one send group
  per subscription inside that domain), the
  publisher's track priority orders streams within a broadcast, and
  downstream subscriber priorities are not forwarded. Attribute FETCH streams
  to a domain the same way: cache-miss and history fetches carry Subscriber
  Priority outside any subscription, so leaving them outside every bucket
  bypasses tenant isolation; settle their carrier beside the subscription one,
  or narrow the goal to subscription delivery.
- Rank by track priority, then subscription, then the subscription's own group
  order (`Subscription::order`, newest first by default).
- Decided 2026-10-20 (maintainer): the publisher's `track::Info::priority`
  breaks a tie between equal subscriber priorities in `Priority::cmp`
  (`rs/moq-net/src/priority.rs`), so the same number decides what to produce
  and, among equal subscriber priorities, what to send first. The
  [ladder controller](/quest/m3/ladder/controller.md) only consumes it.
  `Info::priority` in `rs/moq-net/src/model/track.rs` already claims the
  tiebreak; fix the allocator doc in `rs/moq-net/src/model/bandwidth.rs`,
  which presents send order as unrelated.
- A per-session cap on distinct ranks is a scheduling detail; whatever replaces
  the 255-entry sort must stay O(log n) per group under chat-shaped churn.
- moq-quic already has the fair tier: send domains sharing bytes by deficit
  round robin, with groups inside them. They are crate-private (exercised only
  by its tests) until this quest gives them a consumer. Exposing them also
  gives moq-net's `poll::Session::send_group(priority)` a domain, so land that
  in the same trait break and implementers break once.

Receive ranks, folded in from
[#5878](https://github.com/moq-dev/moq/pull/5878)'s follow-ups by the
maintainer on 2026-10-20. When the receive window is stuck, #5878 evicts the
lowest subscription priority first across every broadcast on the session,
then the group its subscription would deliver last, and sets a stream's rank
once, when its header decodes:

- With fairness on, rank received groups within their broadcast's domain
  first, as sends are, so one broadcast's priorities never evict another's
  partial groups. Received FETCH streams map to a domain through the same
  carrier the send side settles, or fall out with it if the goal narrows to
  subscription delivery.
- Ranking inside domains still leaves the session-wide window to share, so
  a broadcast that fills it would block every other one. Add a fair domain
  tier ahead of the ranks, mirroring the byte-fair send tier: a stuck window
  evicts from the domain holding the most beyond an even share of the window
  across the domains with partial groups, then by rank within that domain.
  A broadcast that fills the window alone may keep it until another arrives.
  A full window admits no newcomer's bytes, so settle what enters a broadcast
  into the fair set first (its subscription, or the header of a stream the
  stuck-window eviction made room for), and test that it gets in.
- Re-rank a receive stream when its subscription's priority or group order
  changes, instead of keeping the rank from its header.

Signed priority, decided with the maintainer:

- moq-lite carries `p + 128` (flip the top bit). The mapping is one-to-one
  and keeps order, so a given byte means what it means today; only the API
  number changes. The default becomes byte 128.
- IETF carries `127 - p`. Both mappings are one-to-one over the whole `i8`
  range, with no saturation. An unset priority goes out as IETF 127, not the
  draft's usual 128, and an absent IETF priority decodes to 0, the unset
  default. Pin both ends in tests.
- Keep one urgency on both wires (#4273), so the IETF byte is always `255 -`
  the lite byte. Mapping IETF as `128 - p` to hit the draft's 128 would break
  that, which is why the default is one step off the draft there.
- hang's built-in priorities (`hang::catalog::PRIORITY`, today all below the
  127 default) move above 0, so hang media outranks a track that never set
  one. Rust and JS keep matching values; the spacing is the implementer's call.
- moq-c's `moq_track_info.priority` has no presence flag, so a zeroed struct
  means least urgent today. With `i8`, zero is the default.
- Look for anything that does arithmetic on priority (the lite send queue, JS
  send-order packing, the bandwidth allocator, the relay's max-of-subscribers)
  and keep its ordering, not just its type.
- moq-archive's `.info` priority follows and breaks in place with the
  archive line's other formats (decided 2026-10-20, maintainer): no format
  version or compatibility mapping for recordings made before the cut.
- The moxygen default (127, #4273) already shipped, so the default byte moves
  a second time. Add an upgrade note to the release's upgrade page saying why.
  Rejected: a mapping that keeps today's bytes (lite byte = p + 127).

Decided 2026-10-20 (maintainer): the `transport::poll` trait breaks land in
order, [receive memory](/quest/m1/quic/receive-memory.md), then
[Rust request credit](/quest/m1/rs-request-credit.md), then this quest, which
adds the send domain last. The publisher serve loops it rewrites already
park one group per subscription for a stalled subscriber
([moq-dev/moq#6140](https://github.com/moq-dev/moq/pull/6140)); keep that
bound. The moq-c half lands on the
[generated C package](/quest/m1/c/README.md), not the frozen hand-written
ABI, so C users break once.

Prove scoping with the existing lite publisher tests extended to two
broadcasts on one session with fairness on: neither starves, a downstream 255
does not change the upstream order, and with fairness off the strict order is
unchanged. On the receive side, with fairness on, a high priority in one
broadcast never makes a stuck window evict another broadcast's partial groups
first, a broadcast that filled the window before another began receiving
yields down to its even share instead of blocking the newcomer, and a priority
or order update moves an already-ranked stream. The io_uring workers apply the same setting. Move
`doc/concept/moq-lite.md` (the 0..255 knob) and `doc/concept/standard.md`
(IETF 128 maps to 127) to the new range and mapping.

Public API impact: `u8` priority fields become `i8` in every language, and
`Subscribe.priority` on a cluster session changes meaning. Wire impact: none
in format, but the default byte moves from 127 to 128 on moq-lite and from 128
to 127 on IETF. Report both with the draft change.

## Required

- [Generated C bindings](/quest/m1/c/README.md) - the `i8` priority reaches C through the generated package, so C users break once
- [Rust request credit](/quest/m1/rs-request-credit.md) - the previous `transport::poll` break; this one adds the send domain on top

## Related

- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - owns the group
  order a subscription asks for
