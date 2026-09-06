# [L] moq-net: ban Hop ID 0 from hop chains

## Goal

Implement and verify the behavior tracked in [#3060](https://github.com/moq-dev/moq/issues/3060)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

`Origin::UNKNOWN` (Hop ID 0) is documented as "no identity", but it is currently legal *inside a hop chain*, and that is where every problem with it comes from. A chain entry that names nobody cannot be filtered on, cannot be loop-detected, and cannot be told apart from another entry that also names nobody. Ban it from chains and those problems stop being cases to handle.

0 stays reserved and keeps its job as the absence marker in the fields that need one (`AnnounceInterest.exclude_hop`, `AnnounceOk.origin`, `RELAY_HOPS`). That reading is unambiguous precisely because no endpoint may adopt 0 as its identity, which `Origin::new` already enforces. What changes is that a chain names real hops only.

#### What this closes

moq-dev/moq#3053: a peer that declares 0 and sends its own HOP\_PATH cannot be filtered on the identity we assigned it. Substituting the assigned id into the chain was implemented during moq-dev/moq#3042 and reverted after producing four defects in four review rounds. With 0 illegal in a chain, a peer with no identity has nothing legal to write as its terminal entry, so that advertisement is a decode-time PROTOCOL\_VIOLATION rather than something to substitute into. The four defects were:

1. Only the terminal entry may be substituted; earlier zeros belong to upstreams the receiver never spoke to.
2. Substituting can construct an invalid chain: `[X, 0]` from a peer assigned `X` becomes `[X, X]`, a PROTOCOL\_VIOLATION for whoever we forward it to.
3. The assigned identity must not be tested against a peer that declared one, or its ordinary traffic is discarded as a loop.
4. The check and the substitution must be gated together, or a peer assigned `Y` that sent `[Y, 0]` bypasses the check and gets rewritten anyway.

Each was a new conditional on the same decision. None of them exist if the chain cannot carry 0.

It also removes the privacy question substitution raised: an assigned identity is indistinguishable on the wire from a declared one, so writing it into a forwarded chain publishes our private name for a peer that asked not to be named.

#### Scope

**Both wire dialects.** A non-zero Hop ID appearing twice is already a PROTOCOL\_VIOLATION in `draft-lcurley-moq-cluster`; moq-dev/moq#3049 mirrors that into `draft-lcurley-moq-lite` and moves it into `OriginList` so it holds at construction rather than only at decode. This issue is the next step on the same rule: `HopPath::validate` drops the duplicate-zeros exemption, and a zero entry becomes invalid on its own.

**moq-lite-01/02/03.** These carry no real Hop IDs. Lite-03 sends a bare hop count that `announce.rs` expands into that many `UNKNOWN` placeholders, which is the only remaining source of zeros in a chain, and lite-01/02 send nothing at all. They stay supported: the count is read as the **route cost** instead, which is what it was for before it was also made to carry loop prevention, and which the lite draft already equates to it (`An absent parameter means the default cost of 1, under which the accumulated Route Cost equals the hop count`).

That means the loop bound has to come from the cost, since the count no longer tracks chain length:

- charge the configured link cost, with a **mandatory floor of 1** on lite-01/02/03 links. A link priced 0 is a supported config (two relays in one datacenter) and would otherwise stop the value growing, leaving the loop unbounded.
- reject a received value above `MAX_HOPS`, reproducing today's 32-hop ceiling.
- emit the accumulated cost where `encode_hops` currently emits `hops.len()`.

Behavior change worth calling out: a lite-03 route's stored chain drops from N entries to one (`[assigned_upstream_id]`), so cost carries the distance and hop length stops standing in for it. That shifts how lite-03 routes rank against others, and the tie-break key in `model/origin.rs` is computed over the chain.

**Drafts.** `draft-lcurley-moq-cluster` section "The Reserved Hop ID 0" is deleted rather than amended. The rules it anchors change with it: HOP\_PATH validity becomes "no Hop ID appears twice" with no exemption, "an advertisement whose first entry is 0 has an unknown origin" goes away because every first entry now names a real publisher, and "Assigned Identities" shifts from MAY to mandatory, since a receiver has no way to spell an unnamed upstream in a chain. `RELAY_HOPS` keeps meaning "no identity" when it carries 0; what a peer may no longer do is put 0 in a HOP\_PATH. `draft-lcurley-moq-lite` gets the matching chain rule alongside the Hop Count rule.

The section's honest summary today, `Declaring 0 therefore trades loop detection and failover for anonymity`, becomes false rather than merely narrower: a peer that declares 0 is assigned an identity and filtered on it, and gets no anonymity from the chain because it can no longer write into one.

#### Done when

Every `== Origin::UNKNOWN` / `!= Origin::UNKNOWN` test that exists to ask "does this chain entry name anybody" is gone, across `lite/subscriber.rs`, `ietf/{subscriber,publisher,cluster}.rs`, and `model/{origin,broadcast}.rs`. The marker survives only where it means "this field is absent". Any survivor in the first category means 0 is still special somewhere and the change is incomplete.

Targets `dev`: it changes published `moq-net` API. The outbound validation it
builds on landed in #3066, where `Hops::push` enforces the duplicate rule
wherever a chain is built.

## Closes

- [#3060](https://github.com/moq-dev/moq/issues/3060) - close this issue when the quest finishes
