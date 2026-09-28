# [M] Redundant ingest under one epoch

## Goal

Decide whether and how two live publishers of identical content share one
broadcast, so viewers survive losing one faster than the QUIC keep-alive.
The study may end in a no-go.

## Plan

Start from what is documented today (`doc/bin/cli.md` "Redundant
publishers"): two encoders sharing a Hop ID (`--hop 42`) are one first hop,
so relays hold both routes and fail over at a group boundary under the #3312
same-first-hop rule, provided the tracks are identical with aligned groups.

Open questions: how that maps onto epochs (the pair claiming one
`@<uuidv7>`), what enforces the alignment the docs only ask for (group
sequences, a matching catalog), and who declares the incumbent dead early: a
failover service that retracts it, or active-active delivery to the relay.
[Cluster routing](/quest/m1/cluster-routing.md) drops hop lists inside a
cluster and must decide what replaces this failover; follow its answer.
Weigh them against the moq-transport rule that multiple publishers of a
namespace must each be asked (#3697) and the cluster draft. Output: a
decision, with a quest for the chosen mechanism.

## Related

- [Cluster routing](/quest/m1/cluster-routing.md) - decides what replaces first-hop failover inside a cluster
- [Broadcast epochs](/quest/m1/broadcast-epoch/README.md) - explicit epochs are what a redundant pair would share
