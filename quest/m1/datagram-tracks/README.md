# Datagram tracks

## Goal

A track is either a datagram track or a stream track, fixed when it is
created. A datagram track:

- is subscribed through its own request, not SUBSCRIBE;
- carries frames that have only a sequence number and a timestamp, with no
  group;
- takes no bounded ranges;
- is never cached, stored, or returned by FETCH.

moq-transport maps its sequence to a group ID with object ID 0. Rust and JS
speak it on lite-07, and the epic ends with a Rust-JS interop test of a
datagram track on lite-07, direct and through a relay. Older lite versions
keep today's datagrams.

## Plan

Decided 2026-11-01 (maintainer, in `/quest-plan`):

- **Datagrams are a track-level decision.** ✅ A track is datagrams or
  streams, never a mix. This reverses the
  [subscribe-ranges](/quest/m1/subscribe-ranges/README.md) decision of
  2026-10-20 that any single-frame group may go as a datagram, including a
  copy of a streamed one. With no mixing, the stream/datagram dedup rule, the
  ordering of a datagram among its group's streams, NOT_FETCHABLE, and every
  datagram case in range accounting all go from lite-07.
  - Reason: datagrams are never cached or stored, so they can't take part in
    ranges, fills, or FETCH. Mixing them into a stream track makes every one
    of those rules carry a datagram exception.
- **Own request.** ✅ A datagram track is subscribed through its own lite-07
  request, not SUBSCRIBE, and that request carries only a priority. ✅ It is
  live from now: no start, end, order, or max delay. An update changes only
  the priority. A datagram still waits behind higher-priority traffic.
- **No group terminology.** ✅ A datagram carries a sequence number and a
  timestamp, with no group. The sequence is the datagram track's own, not
  the group sequence space. On moq-transport it maps to a group ID with
  object ID 0.
  - This exempts datagrams from
    [lite-07 by start](/quest/m1/time-addressing/lite07.md): they keep a
    sequence, while stream tracks address groups by start.
  - The timestamp stays, as it does for every frame.
- **Discovery.** ✅ The application knows which kind a track is, through its
  catalog or by convention. The wire carries no kind signal.
  - ✅ Datagram tracks and stream tracks have separate namespaces, so
    "video" as streams and "video" as datagrams are independent tracks.
  - A datagram request for a name with no datagram track is answered as for
    any missing track.
  - A relay forwards the kind it receives and needs no catalog.
- **Model API.** ✅ Separate types in Rust and JS. A broadcast creates a
  stream track or a datagram producer/consumer pair with its own sequence
  counter. The datagram types have no group, frame, range, fetch, or cache
  API, so the group terminology disappears for datagrams.
- **moq-transport mixing.** ✅ Keep the requested kind.
  - A datagram-track subscription maps to IETF SUBSCRIBE and keeps only
    datagrams (object 0). Stream objects arriving on it are dropped and
    logged.
  - A stream-track subscription drops datagrams.
  - Outbound, a datagram track sends only OBJECT_DATAGRAM with object ID 0.
- **Archive.** ✅ moq-archive skips datagram tracks. A datagram track is
  live-only, and an archive's playback doesn't include it.
- **Version.** ✅ lite-07, before it is finalized.
  [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) requires this epic.
  lite-01..06 keep today's group datagrams.

Not settled by the maintainer, so the first child decides it and records why:
the datagram request's answer carries the track's timescale and
compression, as TRACK_INFO does for a stream track.

This README owns the work no child owns:

- the Rust-JS interop test of a datagram track on lite-07, direct and
  through a relay;
- rewriting the Datagrams section of `doc/concept/moq-lite.md`.

The draft's own Datagrams section changes with the
[Rust child](/quest/m1/datagram-tracks/rust.md).

## Required

- [Rust datagram tracks](/quest/m1/datagram-tracks/rust.md) - Rust model types, the lite-07 datagram request and body, the draft, the relay, and moq-archive skipping datagram tracks
- [JS datagram tracks](/quest/m1/datagram-tracks/js.md) - `@moq/net` datagram types and the lite-07 datagram request
- [moq-transport datagram tracks](/quest/m1/datagram-tracks/ietf.md) - Rust and JS map a datagram track to SUBSCRIBE and OBJECT_DATAGRAM with object ID 0, keeping the requested kind
- [Bindings datagram tracks](/quest/m1/datagram-tracks/bindings.md) - moq-ffi, moq-c, and every wrapper expose the separate datagram types

## Related

- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - ranges no longer cover datagrams
- [Address groups by timestamp](/quest/m1/time-addressing/README.md) - stream tracks address by start; datagram tracks keep a sequence
- [Broadcast archives](/quest/m1/archive/README.md) - skips datagram tracks
