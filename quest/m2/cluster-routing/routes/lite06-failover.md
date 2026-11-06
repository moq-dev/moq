# [M] A declined lite-06 route is reconsidered when the lite-07 path fails

## Goal

In a mixed-version cluster, a relay that declined an identity's lite-06
route because it already knew that identity over lite-07 accepts the lite-06
route again once the lite-07 path is gone, so the lite-06 path takes over
instead of leaving the broadcast unreachable. Today the relay declines it
once and never revisits it.

## Plan

Decided 2026-11-06 (maintainer, /quest-complete, a follow-up of
[moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023)): ✅ m2, in this
epic, since the cut ships mixed-version clusters and this is a failover gap.
Not chosen: m3; m4.

- Recognize known identities on lite-06 ingress, and keep the declined route
  so it can be accepted again when the lite-07 route for that identity ends.
- Cover it with a mixed-version test: a relay with both paths to one
  publisher loses the lite-07 one and keeps serving over lite-06.

Starts once #8023 lands. Public API: none expected. Wire: none.
