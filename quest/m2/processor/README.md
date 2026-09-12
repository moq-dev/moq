# External processors

## Goal

A customer runs a worker in its own environment, connects outbound to a MoQ
deployment, reads only eligible source media, and publishes an on-demand
contribution at `<source>/<processor>.pro`. The platform supplies
registration, scoped credentials, routing, demand, status, and usage
visibility; it does not upload or execute customer code.

This questline holds the protocol and authorization contracts that make the
seam possible. The hosted halves (processor registration, the contribution
service contract, and credential minting) stay downstream in moq.pro. The
contract is not vision-specific: captioning, moderation, telemetry extraction,
and custom transforms use the same worker lifecycle.

## Quests

- [Processor media contract](/quest/m2/processor/media-contract.md) - define
  contribution references, source relations, and correlation in the Hang
  catalog
- [Advertise-only authorization](/quest/m2/processor/advertise-auth.md) - a
  worker may advertise its contribution suffix without receiving permission to
  publish arbitrary matching paths
- [Expiring media grants](/quest/m2/processor/grant-lease.md) - enforce
  short-lived exact grants on already-open consumer and producer handles

## Related

- [Reference vision worker](/quest/m3/processor-vision.md) - a runnable worker
  publishes frame-correlated detections and proves demand, reconnect,
  failover, and teardown end to end
- [Wildcard advertisements](/quest/m2/wildcard/README.md) - lets a dormant
  processor advertise what it could serve without enumerating live sources
