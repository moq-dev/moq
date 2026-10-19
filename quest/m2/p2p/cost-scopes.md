# [S] Peer route policy

## Goal

Application-selected CDN and direct connections feed one shared origin. Its
ordinary priority/cost rules choose the best eligible route, preserving the
CDN session and discovery while avoiding unused duplicate payload pulls.

## Plan

Decided 2026-10-13, replacing the unconditional direct-first policy:

- Applications explicitly choose which discovered peers to connect to and
  assign per-connection priority. Priority wins before configured additive
  cost; links cost one by default and zero is refused (decided 2026-10-13). No automatic dial-all, cache price,
  warm/cold route state, or RTT-based repricing by default.
- Within a priority, compare complete route cost including the origin's
  prefix seed. A direct routing node loses when its total route is more
  expensive. Equal distinct routes retain deterministic path hashing; newest
  wins only for otherwise identical routes, not every new peer connection.
- A browser serves only what it publishes. Native routing nodes may transit
  authorized content when the application turns on the connection's local
  `proxy` preference (decided 2026-10-14): on for redundancy, such as reaching
  the CDN through a peer whose uplink died, off for conference offload.
  Browser tabs stay off until the [Rust-to-TypeScript](/quest/m2/rs2ts/README.md) port. A neighboring watcher is not a source of its received
  media. The authenticated origin identity maps announcements to roster peers.
- Matching explicit epochs permit shared logical-track resume. Different or
  missing epochs require Restart and fresh resolution. Keep CDN control and
  discovery while releasing its unused media demand after P2P handover.
- Active/passive health may temporarily exclude a stalled route and restore
  it after verified recovery, but only for epoch-bound broadcasts. That policy
  belongs to Route health, not a P2P-specific resubscribe loop.

Mirror the application's priority/cost controls in native configuration and
use the shared routing concept terminology. Test a preferred CDN overriding
cheaper P2P, a shorter same-priority peer winning, a longer peer losing,
a neighboring watcher offering nothing it merely receives, reconnect tie
behavior, and fallback retaining the CDN session without duplicate payload.

Public API: application-owned connection priority/cost and `proxy` controls
in Peers and native configuration. Wire: none beyond shared routing and existing resume.

## Required

- [JS track handover](/quest/m1/js-group-handover.md) - same-epoch browser migration
- [Routes and announces](/quest/m1/cluster-routing/routes.md) - common identity and route ordering
- [Signaling and policy](/quest/m2/p2p/signal.md) - explicit application peer selection
- [moq-cli joins](/quest/m2/p2p/cli.md) - matching native controls

## Related

- [Route health](/quest/m2/route-health/README.md) - automatic epoch-only stalled-delivery recovery
