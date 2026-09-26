# Relay drain harness

A viewer watching a live track through a relay that drains moves to a sibling
relay behind the same name without missing a group. This is the whole drain
story at once: the relay's GOAWAY, the JS client's migration on it, and the
origin handing the path over while the old session still serves.

## Running

```bash
just test drain
```

```bash
just test drain --timeout 120
```

`cargo build -p moq-relay` builds the relay from this checkout. `DRAIN_PROFILE`
picks its cargo profile, and `RELAY_BIN` points at a prebuilt relay instead.
Ports come from the shared reservation (see [the harness contract](../README.md)).
A failing run keeps its run directory with both relay logs; a passing run
deletes its own.

## Shape

`run.sh` starts relay B, then relay A clustered to B, and hands both to
`drain.ts`, which plays every other part:

- **The name.** A TCP proxy stands in for DNS. The viewer dials it and lands on
  A. Pointing it at B before draining A is the fleet's DNS withdrawal: nothing
  new resolves to A, while the sessions already there stay.
- **The publisher** sits on B and writes one group every 100 ms, each carrying
  its own sequence number. A pulls the track through the cluster, so both relays
  serve the same groups.
- **The viewer** is `@moq/net` over WebSocket, following whichever broadcast the
  path routes to the way a player does: subscribe to the new one, drop the old.

Once the viewer has read groups through A, the driver withdraws A from the name
and sends it SIGTERM, which fires the same trigger an embedder's drain hook
does. The run passes when:

- the viewer reads fresh groups through B within a few seconds, well inside A's
  20 s drain window, having dialed A exactly once;
- every group from the first read through the last arrived, from one relay or
  the other;
- the viewer leaves A at its 2 s handover cap, and A then exits on its own,
  logging that every session left rather than that its deadline forced one out.

The viewer subscribes with a 1 s latency budget, as the interop subscribers do.
After the swap, B has to subscribe upstream afresh once A drops its pull, and the
budget is what reaches back to a group in flight across the swap. With no budget,
a group boundary that lands inside the swap loses that group.
