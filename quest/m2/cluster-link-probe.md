# [M] Idle cluster links learn their loss before a stream crosses them

## Goal

A cluster link that carries no media is still priced for its loss, so routing
can avoid a lossy edge before the first stream pays to discover it, at a
measured and bounded cost in bytes per second per link.

## Plan

`[cluster.cost]` prices a link from the sender's QUIC counters, and loss only
shows up under traffic: an idle link is priced on its RTT alone, the first
stream takes it, the loss appears within a sampling interval, and the route
moves off it at the next group boundary. That is what the triangle testbed
shows, and it is the cost of measuring nothing while idle.

The PROBE draft (`drafts/draft-lcurley-moq-probe.md`) has the Increase level for
exactly this: the subscriber names a target bitrate and the publisher pads up
to it. `rs/moq-net` implements Report only. Implement padding at a small
target on cluster sessions the link meter holds open, sample the loss it
reveals, and report what it costs: at 1% loss a few hundred packets are needed
per estimate, so a trickle of 100 kbit/s learns a lossy link in tens of
seconds while an idle link costs that much forever. Make the target a
`[cluster.cost]` knob that defaults to off, and extend the testbed's idle-link
bytes column to show the trade.

## Related

- [Bandwidth estimate release](/quest/m2/web-transport-bandwidth-estimate.md) - the same counters feed the bandwidth floor
