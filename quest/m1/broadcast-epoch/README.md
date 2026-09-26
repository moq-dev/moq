# Broadcast epochs

## Goal

Reusing a broadcast name is never ambiguous. By default, each publish of
`demo/BBB.hang` goes out as `demo/BBB.hang/@<uuidv7>`, so the relay and viewers
can tell a new broadcast from a continuation. A viewer of the name follows
the newest live epoch as soon as it is announced, instead of waiting ~30s for
the old route's QUIC idle timeout. Moving to a new epoch is a clean boundary
(a fresh catalog and tracks, never spliced), and each run stays addressable
by its full path.

The epoch rides in the path, so it survives any moq-transport relay, and no
wire message changes. At an epoch-aware relay, a request for a bare name
resolves to its newest live epoch on every protocol version.

Non-goals: redundant publishers sharing one epoch, and failing over between
them faster than the keep-alive (the [redundant ingest](/quest/m2/redundant-ingest.md)
study). Also out of scope: trusting the publisher's clock (a far-future epoch
wins until its route goes away).

## Plan

Decided:

- The marker is a child segment `@<uuidv7>`, parsed into
  `Option<Epoch>` by [the primitive](/quest/m1/epoch.md). Not a lite-07 flag:
  the path is the only carrier.
- The broadcast-publish path mints an epoch unless the path already carries
  one. A caller who passes an explicit epoch, such as a redundant publisher,
  keeps it. The prefix-route `announce(prefix, route)` stays raw, and that is
  the opt-out. So prefix vs broadcast is an API distinction, not a wire flag.
- Viewers follow the greatest epoch with a live route. When it goes away and an
  older one is still live, they fall back to it.
- A bare request with no route of its own resolves to that same epoch on every
  version, so lite-06 and IETF clients keep working through an epoch-aware
  relay. When a newer epoch appears, the bare subscription ends with a typed
  reset, and the client's normal resubscribe lands on the new one.
- An unmodified third-party relay routes `foo/@<epoch>` but never resolves a
  bare `foo`, since a route covers its descendants, not its parent. A
  bare-name viewer behind one needs a publisher that opts out with the raw
  prefix route. Document this rather than promise it works.
- Derived output mirrors the epoch it came from
  (`.transcode/pid/foo.hang/@e`, per the
  [wildcard](/quest/m1/wildcard/README.md) line's derived-output layout), so
  the service's prefix claim still covers it.

This README owns:

- An end-to-end relay test: republish a name while the old publisher's session
  stays open. A new-API viewer and a lite-06 or IETF bare-path viewer both
  reach the new epoch within one RTT-scale bound rather than the idle timeout.
  Killing the newest epoch falls back to a still-live older one.
- A `doc/concept` page on broadcast naming: what an epoch is, publish and
  consume behavior, takeover and fallback, bare-path resolution, and the
  prefix-route opt-out.

## Quests

- [Origin](/quest/m1/broadcast-epoch/origin.md) - moq-net publish mints an epoch, consumers follow the newest live one, and bare requests resolve to it on every version
- [Apps](/quest/m1/broadcast-epoch/apps.md) - moq-cli, the browser publish and watch components, and demo/web publish under epochs and play bare names
- [Gateways](/quest/m1/broadcast-epoch/gateways.md) - RTMP, SRT, and WHIP ingest mint an epoch per incoming connection, so an encoder reconnect is a clean takeover
- [Bindings](/quest/m1/broadcast-epoch/bindings.md) - moq-ffi, libmoq, and every wrapper expose the epoch and inherit the default
- [GStreamer and OBS](/quest/m1/broadcast-epoch/gst-obs.md) - moqsink and the OBS plugin publish each run under a fresh epoch

## Required

- [Epoch primitive](/quest/m1/epoch.md) - the shared `Epoch` type and path split
