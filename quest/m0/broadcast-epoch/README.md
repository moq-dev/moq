# Broadcast epochs

## Goal

A path, with its `@epoch`, is the only content identity, and no first-party
publisher reuses one for different content. With #4741, any route covering a
path resumes its subscriptions from the first frame the subscriber lacks,
whoever serves it. So epochs are a correctness requirement: a publisher that
restarts its group numbering at 0 under an un-epoched name, while its old
route lingers, is resumed into the old broadcast and stalls viewers until its
sequence catches up.

By default, each publish of `demo/BBB.hang` goes out as
`demo/BBB.hang/@<uuidv7>`, so a restart is a new broadcast. A viewer of the
bare name follows the newest live epoch as soon as it is announced. Moving to
a new epoch is a clean boundary (a fresh catalog and tracks, never resumed
across), and each run stays addressable by its full path.

The epoch rides in the path, so it survives any moq-transport relay, and no
wire message changes. At an epoch-aware relay, a request for a bare name
resolves to its newest live epoch on every protocol version.

Non-goals: pooling, which needs nothing here, since every publisher of one
path is already one source; a redundant pair shares an explicit epoch through
[`--hop` removal](/quest/m0/broadcast-epoch/hop-removal.md). Also out of
scope: trusting the publisher's clock (a far-future epoch wins until its
route goes away).

## Plan

Decided:

- This line gates the next release (decided 2026-10-03: #4741 can merge to
  main, but without epochs every restarting first-party publisher stalls its
  viewers).
- The marker is a child segment `@<uuidv7>`, parsed into
  `Option<Epoch>` by [the shared primitive](/doc/concept/moq-lite.md#publisher-epochs). Not a lite-07 flag:
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
- Publishers on the default publish path, such as moq-boy and moq-room,
  inherit the epoch from Origin. moq-stats mints its own through
  [Stats epochs](/quest/m1/stats-epoch.md), which also gates the release
  (decided 2026-10-04): a restarted stats node under a reused name stalls its
  viewers the same way.
- [Retracted demand release](/quest/m1/unannounce-demand-release.md) also
  gates the release (decided 2026-10-05): a regression from #4741 on main that
  `release` lacks.
- Derived output mirrors the epoch it came from
  (`.pro/transcode/<pid>/foo.hang/@e`, per the
  [wildcard](/quest/m0/wildcard/README.md) line's derived-output layout), so
  the service's prefix claim still covers it.

This README owns:

- An end-to-end relay test: republish a name while the old publisher's session
  stays open. A new-API viewer and a lite-06 or IETF bare-path viewer both
  reach the new epoch within one RTT-scale bound rather than the idle timeout.
  Killing the newest epoch falls back to a still-live older one.
- A `doc/concept` page on broadcast naming: what an epoch is, publish and
  consume behavior, takeover and fallback, bare-path resolution, and the
  prefix-route opt-out.

## Required

- [Origin](/quest/m0/broadcast-epoch/origin.md) - moq-net publish mints an epoch, consumers follow the newest live one, and bare requests resolve to it on every version
- [Apps](/quest/m0/broadcast-epoch/apps.md) - moq-cli, the browser publish and watch components, and demo/web publish under epochs and play bare names
- [Gateways](/quest/m0/broadcast-epoch/gateways.md) - RTMP, SRT, and WHIP ingest mint an epoch per incoming connection, so an encoder reconnect is a clean takeover
- [TS restart](/quest/m0/broadcast-epoch/ts-restart.md) - a signalled backward TS discontinuity finishes the broadcast and continues the same input under a fresh epoch
- [Bindings](/quest/m0/broadcast-epoch/bindings.md) - moq-ffi and every wrapper expose the epoch and inherit the default
- [GStreamer and OBS](/quest/m0/broadcast-epoch/gst-obs.md) - moqsink and the OBS plugin publish each run under a fresh epoch
- [Remove `--hop`](/quest/m0/broadcast-epoch/hop-removal.md) - `moq` takes an optional `--epoch` instead of `--hop`, a plain publisher declares a random Hop ID, and the per-session hop stamp and NO_CAPACITY are gone
- [Stats epochs](/quest/m1/stats-epoch.md) - moq-stats publishes each node under its own epoch, so a restarted node never stalls its viewers
- [Retracted demand release](/quest/m1/unannounce-demand-release.md) - a retracted broadcast's track demand is released when its last subscriber leaves, as before #4741
