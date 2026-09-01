# [S] Lite draft routing text

## Goal

`drafts/draft-lcurley-moq-lite.md` describes the routing a relay actually
performs. Today it normatively specifies a warm-cost discount and an adoption
rank that no implementation has, which misleads anyone building against the
draft.

## Plan

[moq#3225](https://github.com/moq-dev/moq/pull/3225) dropped the warm-cost
discount, the handover hold, and the `(cold, hash)` adoption gate, and forwards
accumulated costs only. It left the draft's text describing all of it, so the
spec and the code now disagree about behavior another implementation would
interop against.

Bring the text down to what is implemented:

- The actively-carrying discount (currently a MAY, with its ceiling exemption
  and the paragraph about a drain propagating through carrying relays).
- The sentence in the changelog entry stating that a relay adopts another
  carrying relay only when that relay's `(Cold cost, hash)` rank is strictly
  lower.

Keep the `Warm` and `Cold` fields themselves. They are on the wire, decoded,
and ranked on: `route_order` still breaks a warm tie on the lower cold cost,
and a wire that cannot express cold still reads as the saturation ceiling. Keep
the selection paragraph as it stands, including preferring the most specific
covering prefix ahead of cost, which is what `best_server` does.

Describe the removal as a changelog entry rather than editing history, per how
the draft records its own evolution.

[Warm advertise](/quest/m2/pop-skipping/warm-advertise.md) and
[Rank](/quest/m2/pop-skipping/rank.md) put the text back as they land, each in
its own PR, per the repository rule that a wire or behavior change updates its
draft in the same change. This quest exists so the draft is not wrong in the
meantime.

Validate with `just drafts check`. The fix lands on `dev`, where the divergence
is.

## Related

- [Warm advertise](/quest/m2/pop-skipping/warm-advertise.md) - re-specifies the
  discount when it is implemented again
