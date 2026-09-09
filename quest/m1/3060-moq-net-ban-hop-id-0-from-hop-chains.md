# [L] moq-net: ban Hop ID 0 from hop chains

## Goal

A hop chain names real hops only. `Hop::UNKNOWN` (Hop ID 0) stays the absence
marker in the fields that need one (`AnnounceInterest.exclude_hop`,
`AnnounceOk.origin`, `RELAY_HOPS`), but a chain entry that names nobody is a
decode-time PROTOCOL_VIOLATION on both wire dialects, so nothing has to
filter, loop-detect, or substitute around it.

## Plan

`Hop::UNKNOWN` (rs/moq-net/src/model/origin.rs:35, re-exported at
model/mod.rs:49) is documented as "no identity" but is legal inside a hop
chain, and that is where every problem with it comes from. A chain entry that
names nobody cannot be filtered on, cannot be loop-detected, and cannot be
told apart from another entry that also names nobody. 0 keeps its job as the
absence marker because no endpoint may adopt it as an identity, which
`Hop::new` already enforces. What changes is that a chain names real hops
only.

### What this supersedes

#3053 (closed): a peer that declares 0 and sends its own HOP_PATH cannot be
filtered on the identity we assigned it. Substituting the assigned id into
the chain was implemented during #3042 and reverted after producing four
defects in four review rounds:

1. Only the terminal entry may be substituted; earlier zeros belong to
   upstreams the receiver never spoke to.
2. Substituting can construct an invalid chain: `[X, 0]` from a peer assigned
   `X` becomes `[X, X]`, a PROTOCOL_VIOLATION for whoever we forward it to.
3. The assigned identity must not be tested against a peer that declared
   one, or its ordinary traffic is discarded as a loop.
4. The check and the substitution must be gated together, or a peer assigned
   `Y` that sent `[Y, 0]` bypasses the check and gets rewritten anyway.

Each was a new conditional on the same decision. None of them exist if the
chain cannot carry 0. Substitution also raised a privacy question: an
assigned identity is indistinguishable on the wire from a declared one, so
writing it into a forwarded chain publishes our private name for a peer that
asked not to be named. One such substitution still ships: lite/subscriber.rs:
296-307 rewrites the first `Hop::UNKNOWN` placeholder with `session_origin`
through `replace_first`, and the chain that leaves it no longer records that
the entry was assigned rather than declared.

### Scope

**Both wire dialects.** A non-zero Hop ID appearing twice is already a
PROTOCOL_VIOLATION on both; #3066 moved the rule into `Hops` so it holds at
construction rather than only at decode. The zero exemption lives there too:
`Hops::push` (model/origin.rs:277) and `TryFrom<Vec<Hop>>` (:340-345) skip
the duplicate check for `Hop::UNKNOWN`. Drop the exemption and refuse a zero
entry outright. `HopPath::validate` (ietf/cluster.rs:88-93) only rejects an
empty list and stays as it is.

**moq-lite-01/02/03.** These carry no real Hop IDs. Lite-03 sends a bare hop
count that lite/announce.rs:195 expands into that many `Hop::UNKNOWN`
placeholders, the only remaining source of zeros in a chain; lite-01/02 send
nothing at all. They stay supported: the count is read as the route cost
instead, which is what it was for before it was also made to carry loop
prevention, and which the lite draft already equates to it: "the accumulated
Route Costs equal the hop count and routing degenerates to shortest-path"
(drafts/draft-lcurley-moq-lite.md:789). Lite-06 carries a Warm and a Cold
Route Cost (:859-862, :888-889), so the lite-03 count feeds both halves
through `Cost::new` (model/origin.rs:449).

The loop bound then has to come from the cost, since the count no longer
tracks chain length:

- charge the configured link cost, with a mandatory floor of 1 on
  lite-01/02/03 links. A link priced 0 is a supported config (two relays in
  one datacenter) and would otherwise stop the value growing, leaving the
  loop unbounded.
- reject a received value above `MAX_HOPS` (model/origin.rs:211), reproducing
  today's 32-hop ceiling.
- emit the accumulated cost where `encode_hops` (lite/announce.rs:224)
  currently emits `hops.len()`.

Behavior change worth calling out: a lite-03 route's stored chain drops from
N entries to one (`[assigned_upstream_id]`), so cost carries the distance and
hop length stops standing in for it. That shifts how lite-03 routes rank
against others: `route_order` (model/origin.rs:633-639) ranks cost first,
then chain length, then a hash over the chain.

**Drafts.** `draft-lcurley-moq-cluster` section "The Reserved Hop ID 0"
(drafts/draft-lcurley-moq-cluster.md:108) is deleted rather than amended. The
rules it anchors change with it: HOP_PATH validity becomes "no Hop ID appears
twice" with no exemption, "an advertisement whose first entry is 0 has an
unknown origin" goes away because every first entry now names a real
publisher, and "Assigned Identities" (:121) shifts from MAY to mandatory,
since a receiver has no way to spell an unnamed upstream in a chain.
`RELAY_HOPS` keeps meaning "no identity" when it carries 0; what a peer may no
longer do is put 0 in a HOP_PATH. `draft-lcurley-moq-lite` gets the matching
chain rule alongside the Hop Count rule and drops "Duplicate values of 0 are
not a violation" (draft-lcurley-moq-lite.md:886).

The cluster section's summary, "Declaring 0 therefore trades loop detection
and failover for anonymity", becomes false rather than merely narrower: a
peer that declares 0 is assigned an identity and filtered on it, and gets no
anonymity from the chain because it can no longer write into one.

### Done when

Every `== Hop::UNKNOWN` / `!= Hop::UNKNOWN` test that exists to ask "does this
chain entry name anybody" is gone, across lite/subscriber.rs,
lite/publisher.rs:612, ietf/subscriber.rs, ietf/publisher.rs,
ietf/cluster.rs, and model/origin.rs. The marker survives only where it means
"this field is absent" (server.rs:404 is one). Any survivor in the first
category means 0 is still special somewhere and the change is incomplete.

Targets `dev`: it changes published `moq-net` API.

## Closes

- [#3060](https://github.com/moq-dev/moq/issues/3060) - close this issue when the quest finishes

## Related

- [#3053](https://github.com/moq-dev/moq/issues/3053) - closed; the substitution approach this supersedes
