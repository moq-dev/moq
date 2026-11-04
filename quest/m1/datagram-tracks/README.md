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
  live from now: no start, end, order, or `Delay`. An update changes only
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
- **Old versions carry both kinds.** ✅ Decided 2026-11-03 (maintainer, on
  [moq-dev/moq#7752](https://github.com/moq-dev/moq/pull/7752)): "carry both
  on old versions, like moq-transport. Ignore duplicates because in practice,
  only one was being used anyway."
  - A lite-05/06 SUBSCRIBE for a name carries its stream track's groups and
    its datagram track's datagrams, as one moq-transport subscription does. A
    name with only a datagram track is served from it alone.
  - A subscriber keeps the kind it asked for. One upstream SUBSCRIBE serves
    readers of both kinds: the stream track's subscription feeds the name's
    datagram track, so a relay between old peers carries datagrams without a
    second subscription.
  - No dedup: a name using both kinds can deliver a datagram to an old
    subscriber that also holds a group of the same sequence.
  - Reason: lite-05/06 are the default connections, and moq-ffi/moq-c publish
    every raw track's datagrams beside it, so serving a name only one kind
    stopped their datagrams on the default path.
- **Old-version lookup.** ✅ Decided 2026-11-04 (maintainer, for
  [moq-dev/moq#7847](https://github.com/moq-dev/moq/pull/7847)): change
  Rust's lite-05/06 on-demand lookup to discover datagrams without waiting
  for a stream-handler answer, keeping the independent binding request API.
  Not chosen: requiring stream handlers to reject missing names, with a
  not-found rejection added to the bindings.
  - A published stream track still serves the name, with its datagram track
    beside it. Otherwise a local datagram track answers as soon as its
    handler does. Of a name whose two kinds are both requested on demand,
    the first answer serves it.
  - A session's broadcast is asked for datagrams only once its stream track
    is not found: an old session answers a datagram request by subscribing to
    the stream track, which a fetch-only or query-only relay must not do.
  - Reason: a stream handler need not answer a name it doesn't serve, and
    the bindings' rejection carries only an application error.
- **Version.** ✅ lite-07, before it is finalized.
  [Finalize moq-lite-07](/quest/m1/lite07-finalize.md) requires this epic.
  lite-01..06 keep today's group datagrams.

Decided by the Rust child ([moq-dev/moq#7752](https://github.com/moq-dev/moq/pull/7752)),
not the maintainer, so open to revisiting:

- **The answer.** The lite-07 request (Datagrams Stream 0x9: DATAGRAMS, then
  DATAGRAMS_UPDATE) is answered with DATAGRAMS_INFO, carrying the publisher
  priority and the timescale, and no compression: a datagram track is never
  compressed. Why: a subscriber needs the timescale to read a datagram (its
  Timestamp is present only on a timed track), so the answer must carry it,
  and a separate TRACK lookup would need a kind signal TRACK does not have. A
  payload under 1200 bytes, compressed alone with a fresh window, saves little,
  while compression would cost every datagram an Inflated Size and a decoder.
- **Timestamp on a `Timescale` 0 track.** The datagram carries none, and its
  Sequence stands in for its time. Why: it mirrors the
  [model](/quest/m1/time-addressing/model.md)'s rule that a scale-less frame's
  timestamp is its group's key, since a datagram's address is its sequence.
  Until that model lands, Rust's datagram writes take `Option<Timestamp>` like
  frame writes do (`None` on an untimed track), and the model quest converts
  both.

This README owns the work no child owns: the Rust-JS interop test of a
datagram track on lite-07, direct and through a relay.

## Required

- [JS datagram tracks](/quest/m1/datagram-tracks/js.md) - `@moq/net` datagram types and the lite-07 datagram request
- [moq-transport datagram tracks](/quest/m1/datagram-tracks/ietf.md) - Rust and JS map a datagram track to SUBSCRIBE and OBJECT_DATAGRAM with object ID 0, keeping the requested kind

## Related

- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - ranges no longer cover datagrams
- [Address groups by timestamp](/quest/m1/time-addressing/README.md) - stream tracks address by start; datagram tracks keep a sequence
- [Wrapper traits over moq-net](/quest/m2/net-wrap/README.md) - the traits mirror the separate datagram types
- [moq-lite session tails count drops](/quest/m2/lite-tail-drop-count.md) - datagram holes matter only on lite-05/06
