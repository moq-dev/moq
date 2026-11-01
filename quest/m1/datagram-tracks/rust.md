# [L] Rust datagram tracks

## Goal

moq-net has separate datagram types: a broadcast creates a datagram
producer/consumer pair with its own sequence counter, and stream tracks
carry no datagram API. Lite-07 subscribes to a datagram track through its
own request and sends datagrams addressed by that sequence. The relay
forwards datagram tracks, and moq-archive skips them. lite-01..06 keep
today's group datagrams.

## Plan

The decisions are in the [epic](/quest/m1/datagram-tracks/README.md).

**Model**
- Add the datagram producer and consumer types to `rs/moq-net/src/model/`.
  Each has its own sequence counter and a tiny send/receive buffer, with no
  cache and no ranges.
- Datagram tracks have their own namespace in a broadcast.
- Remove the datagram paths from stream tracks:
  - `insert_datagram` and the stored datagrams in `model/datagram.rs`;
  - `poll_recv_datagram` and its resume handling in `model/resume.rs`;
  - datagram cases in fills, misses, and range accounting.

**Wire (lite-07)**
- A datagram subscribe request carries only a priority, and an update
  changes only the priority.
- A datagram carries its sequence, its timestamp, and the payload.
- Decide whether the request's answer carries the track's timescale and
  compression as TRACK_INFO does, and record the reason in the epic.
- Remove the lite-07 rules that mixing needed:
  - stream/datagram dedup;
  - ordering a datagram among its group's streams;
  - NOT_FETCHABLE;
  - datagrams in SUBSCRIBE_DROP coverage;
  - the datagram grace in the subscriber's end (`a_lost_datagram_delays_the_end`).
- On lite-01..06, map today's group datagrams onto the datagram types:
  - Inbound, a group datagram on a subscription goes to the datagram track of
    the same name.
  - Outbound, a datagram track's subscriber on an old version gets group
    datagrams keyed by its sequence.

**Elsewhere**
- The relay forwards each kind as it received it.
- moq-archive's producer and consumer drop their datagram form.
- moq-ffi keeps its public datagram surface, mapped onto the new types
  internally. Its public API changes in
  [bindings](/quest/m1/datagram-tracks/bindings.md).
- Update `drafts/draft-lcurley-moq-lite.md`: the Datagrams section, the
  request, the error table, and the lite-07 changelog. Then run
  `just drafts check` and `just test interop --all`.
- The PR's Impact section says how to migrate.

Public API: breaking. Stream tracks lose their datagram methods, and the
datagram types are new. Wire: lite-07-wip datagrams and a new request.

## Related

- [Lite-07 by start](/quest/m1/time-addressing/lite07.md) - stream groups address by start; datagrams keep a sequence
