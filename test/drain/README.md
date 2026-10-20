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
- **The viewer** is `@moq/net` over WebSocket. It requests the path once and
  reads one subscription for the whole run. The publisher mints an epoch, so
  B's route serves the same broadcast as A's, and the origin resumes the track
  there from the first group the viewer lacks. Epochs travel on the
  work-in-progress moq-lite-07 only, so the relays, A's cluster dial, and both
  JS sessions offer it.

Once the viewer has read groups through A, the driver withdraws A from the name
and sends it SIGTERM, which fires the same trigger an embedder's drain hook
does. The run passes when:

- the viewer reads fresh groups through B within a few seconds of leaving A,
  well inside A's 20 s drain window, having dialed A exactly once;
- every group from the first read through the last arrived exactly once, from
  one relay or the other, on the one subscription, whose broadcast handle never
  changed;
- the viewer leaves A at its 2 s handover cap, and A then exits on its own,
  logging that every session left rather than that its deadline forced one out.

The viewer subscribes with no latency budget: only the live edge. B serves the
track while A still does, and each group the viewer has not read yet comes
from one route or the other, so none is lost across the swap.
