# [M] Prove complete and fresh route handoff

## Goal

A next-hop change adopts complete, current origin-owned claims without
reviving a known withdrawal or reporting a false publisher END. The chosen
protocol and controlled-time acceptance fixtures are recorded in routes.md
before its wire is written. This is a design/proof quest, not a production
implementation.

## Plan

The existing simulator found 2,000 client updates against a 900 once-only
minimum on END, including 550 re-announcements, and 722.8 aggregate seconds
of false absence after an upstream-link loss. Next-hop authority alone did
not meet the goal; the earlier epic's assertion that it did is withdrawn.

Decided 2026-10-13: keep known advertisement presence through uncertain
handoff, represent forwarding availability separately, and bound retained
state. Route loss is not withdrawal by the origin. An END withdraws that
origin's claim, including wildcard capability claims, not every replica of
an epoch. No promise of learning an unseen withdrawal during a partition.

Decided 2026-10-13: no origin-confirmed reconciliation, since a relay never
waits on an origin round trip before trusting a new next hop. Prove the
remaining candidate against its failing control:

- Origin-versioned claim changes with scoped complete snapshots and retained
  freshness floors. A snapshot must certify both its coverage and which
  changes it incorporates; a high revision for one path proves nothing about
  an unwatched path. This is the candidate to prove.
- A neighbor snapshot barrier alone is a failing control: completeness does
  not reject an older complete snapshot after an END arrived elsewhere.

Prove delayed snapshot/delta reconciliation, partial and widening interest,
cancellation/rejoin, wildcard coverage, origin restart, and stale replay after
collection. Fresh routing identity per restart is settled; exact freshness
representation is not. Compact negative history only when equivalent knowledge
survives, or require fresh reconciliation. Do not retain every historical
neighbor forever or use arbitrary tombstone expiry as a correctness argument.

Keep a live same-epoch route serving while its replacement is validated;
epochless changes retain Restart. A lack of a usable route must be represented
honestly without terminally retracting known presence. Measure client events,
control bytes, replay work, and retained state against active origins, scopes,
claims, and neighbors. Store public synthetic fixtures in CI with mocked time.

Bring the comparison and any remaining wire/API decision to the maintainer,
then update routes.md. The proof must also specify eventual recovery when a
bounded seqno-request attempt ends and no further unsolicited update arrives.

## Required

- [Routing contracts](/quest/m1/cluster-routing/contracts.md) - incarnation, authority, and export views the handoff preserves

## Related

- [Lazy discovery](/quest/m1/lazy-discovery/README.md) - interest changes must not erase freshness protection
