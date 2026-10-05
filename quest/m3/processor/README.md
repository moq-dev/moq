# External processors

## Goal

A customer runs a worker in its own environment, connects outbound to a MoQ
deployment, reads only eligible source media, and publishes an on-demand
contribution under the processor's own prefix, mirroring the source path
(for example `.pro/<processor>/<source path>`, the
[wildcard](/quest/m0/wildcard/README.md) line's derived-output layout). The
platform supplies
registration, scoped credentials, routing, demand, status, and usage
visibility; it does not upload or execute customer code.

This questline holds the protocol and authorization contracts that make the
seam possible. The hosted halves (processor registration, the contribution
service contract, and credential minting) stay downstream in moq.pro. The
contract is not vision-specific: captioning, moderation, telemetry extraction,
and custom transforms use the same worker lifecycle.

## Plan

Decided: derived output follows [Wildcard](/quest/m0/wildcard/README.md)'s
service-prefix layout, not `<source>/<processor>.pro`. Processor output does
not use suffix routing ([announcement shapes](/quest/m2/announce-shapes.md)
may add it to moq-lite separately), and a prefix claim needs the variable part of the path
trailing, so the processor claims its prefix and mirrors the source path
beneath it. The source's catalog reaches the output through a
cross-broadcast reference ([media contract](/quest/m3/processor/media-contract.md)).

Deferred in the 2026-09-30 audit and moved to m3 in the 2026-10-05 audit: no processor customer is committed,
and its end-to-end proof (processor-vision) was deleted.

## Required

- [Processor media contract](/quest/m3/processor/media-contract.md) - define
  contribution references, source relations, and correlation in the Hang
  catalog
- [Advertise-only authorization](/quest/m3/processor/advertise-auth.md) - a
  worker may advertise its service prefix without receiving permission to
  publish arbitrary paths under it
- [Expiring media grants](/quest/m3/processor/grant-lease.md) - enforce
  short-lived exact grants on already-open consumer and producer handles

## Related

- [Wildcard advertisements](/quest/m0/wildcard/README.md) - lets a dormant
  processor advertise what it could serve without enumerating live sources,
  and sets the service-prefix layout
