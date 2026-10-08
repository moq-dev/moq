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

- One `PriorityQueue` per moq-lite session (`lite/publisher.rs`), a strict
  global sort over every in-flight group on the connection: a subscription at
  priority 200 pre-empts one at 100 indefinitely, and only the top 255 groups
  get a distinct rank.
- `Priority` already ranks a group by its track priority, then its subscription
  id, then newest group first within that subscription (`lite/priority.rs`). JS
  packs track priority and the group's position within its own subscription
  into one send order (`js/net/src/lite/priority.ts`), so two tracks at equal
  priority interleave rather than one draining first.
- Group order within a track is fixed newest first today; a subscription-chosen
  order arrives with [Subscribe ranges](/quest/m1/subscribe-ranges/README.md).
- A relay forwards the max of its downstream subscriber priorities upstream
  (`model/subscription.rs`, `lite/subscriber.rs`), never the publisher's track
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
subscriptions or broadcasts must never buy more bandwidth than the bucket
allows.

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
  byte-fair (the scheduler's fair tier, one send group per broadcast), the
  publisher's track priority orders streams within a broadcast, and
  downstream subscriber priorities are not forwarded. Attribute FETCH streams
  to a domain the same way: cache-miss and history fetches carry Subscriber
  Priority outside any subscription, so leaving them outside every bucket
  bypasses tenant isolation; settle their carrier beside the subscription one,
  or narrow the goal to subscription delivery.
- Rank by track priority, then subscription, then the subscription's own group
  order (newest first until subscribe ranges adds a direction).
- Decide whether the publisher's `track::Info::priority` breaks a tie
  between equal subscriber priorities in `Priority::cmp`
  (`rs/moq-net/src/lite/priority.rs`). The
  [ladder controller](/quest/m3/ladder/controller.md), now in m3, wants that
  tiebreak; this quest owns the answer and the code change, so the controller
  only consumes it (2026-10-06 audit). Either way, make the docs agree:
  `Info::priority` in `rs/moq-net/src/model/track.rs` already claims the
  tiebreak, and the allocator doc in `rs/moq-net/src/model/bandwidth.rs`
  presents send order as unrelated.
- A per-session cap on distinct ranks is a scheduling detail; whatever replaces
  the 255-entry sort must stay O(log n) per group under chat-shaped churn.

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
- moq-archive's `Info::priority` follows. Its version-1 `.info` stores the
  `u8`, so existing recordings must keep their meaning: store the moq-lite
  byte (`p + 128`) or bump the format version, never reinterpret silently.
- The moxygen default (127, #4273) already shipped, so the default byte moves
  a second time. Add an upgrade note to the release's upgrade page saying why.
  Rejected: a mapping that keeps today's bytes (lite byte = p + 127).

Prove scoping with the existing lite publisher tests extended to two
broadcasts on one session with fairness on: neither starves, a downstream 255
does not change the upstream order, and with fairness off the strict order is
unchanged. The io_uring workers apply the same setting. Move
`doc/concept/moq-lite.md` (the 0..255 knob) and `doc/concept/standard.md`
(IETF 128 maps to 127) to the new range and mapping.

Public API impact: `u8` priority fields become `i8` in every language, and
`Subscribe.priority` on a cluster session changes meaning. Wire impact: none
in format, but the default byte moves from 127 to 128 on moq-lite and from 128
to 127 on IETF. Report both with the draft change.

## Required

- [Hierarchical stream scheduling](/quest/m1/quic/scheduler.md) - the fair
  tier the per-broadcast send groups ride on

## Related

- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - owns the group
  order a subscription asks for
