# [M] Redundant ingest under one epoch

## Goal

Decide whether and how two live publishers of identical content share one
broadcast, so viewers survive losing one faster than the QUIC keep-alive.
Epochs let the publishers claim one identity (the same `@<uuidv7>`), but
today's #3312 rule splices only across routes with the same first hop, so
two ingest hosts are two identities. The study may end in a no-go.

## Plan

Open questions: whether an explicitly shared epoch may splice across first
hops at a group boundary, and what makes that safe (group sequences aligned
across encoders, a matching catalog). Also who declares the incumbent dead
early: a failover service that retracts it, or active-active delivery to the
relay. Weigh them against the moq-transport rule that multiple publishers of
a namespace must each be asked (#3697) and the cluster draft. Output: a
decision, with a quest for the chosen mechanism.

## Related

- [Broadcast epochs](/quest/m1/broadcast-epoch/README.md) - explicit epochs are what a redundant pair would share
