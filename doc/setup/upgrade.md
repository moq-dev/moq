---
title: Upgrade
description: The order to roll out a release across relays, auth servers, and clients, and each package's migration guide
---

# Upgrade

Every hand-written published package keeps a `MIGRATE.md` saying how to
migrate past each breaking release, newest first. Its section is written when
that release is cut, not as changes land on `main`, so a build from `main` can
be ahead of its guide. The links below track `main`: apply the sections from the
version you run up to the one you install. Most binding users install a
wrapper, such as `moq-rs` on PyPI or `dev.moq:moq` on Maven, whose guide is in
the index below. The raw UniFFI
packages published as `moq-ffi` have no guide of their own:
[moq-ffi's](https://github.com/moq-dev/moq/blob/main/rs/moq-ffi/MIGRATE.md)
covers that layer, in UniFFI names. This page covers what spans packages: the
wire changes that decide the order to upgrade a deployment.

A released flag, environment variable, or config key that was renamed is
refused at startup with its replacement named, rather than ignored. Fix what the
error lists and rerun. Any other `MOQ_*` variable a binary does not read, such as
moq-cli's removed `MOQ_HOP`, is refused at startup too; its guide says what
replaces it.

## Migration guides

- **Binaries:** [moq-relay](https://github.com/moq-dev/moq/blob/main/rs/moq-relay/MIGRATE.md), [moq-cli (`moq`)](https://github.com/moq-dev/moq/blob/main/rs/moq-cli/MIGRATE.md), [moq-gst](https://github.com/moq-dev/moq/blob/main/rs/moq-gst/MIGRATE.md), [obs-moq](https://github.com/moq-dev/moq/blob/main/cpp/obs/MIGRATE.md)
- **Bindings:** [moq-ffi (UniFFI)](https://github.com/moq-dev/moq/blob/main/rs/moq-ffi/MIGRATE.md), [moq-c (C)](https://github.com/moq-dev/moq/blob/main/rs/moq-c/MIGRATE.md), [Python](https://github.com/moq-dev/moq/blob/main/py/moq-rs/MIGRATE.md), [Go](https://github.com/moq-dev/moq/blob/main/go/wrapper/MIGRATE.md), [Swift](https://github.com/moq-dev/moq/blob/main/swift/MIGRATE.md), [Kotlin](https://github.com/moq-dev/moq/blob/main/kt/moq/MIGRATE.md), [Dart](https://github.com/moq-dev/moq/blob/main/dart/moq/MIGRATE.md), [C++](https://github.com/moq-dev/moq/blob/main/cpp/moq/MIGRATE.md)
- **Rust:** [hang](https://github.com/moq-dev/moq/blob/main/rs/hang/MIGRATE.md), [kio](https://github.com/moq-dev/moq/blob/main/rs/kio/MIGRATE.md), [kio-macros](https://github.com/moq-dev/moq/blob/main/rs/kio/macros/MIGRATE.md), [moq-archive](https://github.com/moq-dev/moq/blob/main/rs/moq-archive/MIGRATE.md), [moq-audio](https://github.com/moq-dev/moq/blob/main/rs/moq-audio/MIGRATE.md), [moq-auth](https://github.com/moq-dev/moq/blob/main/rs/moq-auth/MIGRATE.md), [moq-boy](https://github.com/moq-dev/moq/blob/main/rs/moq-boy/MIGRATE.md), [moq-e2ee](https://github.com/moq-dev/moq/blob/main/rs/moq-e2ee/MIGRATE.md), [moq-hls](https://github.com/moq-dev/moq/blob/main/rs/moq-hls/MIGRATE.md), [moq-ice](https://github.com/moq-dev/moq/blob/main/rs/moq-ice/MIGRATE.md), [moq-json](https://github.com/moq-dev/moq/blob/main/rs/moq-json/MIGRATE.md), [moq-loc](https://github.com/moq-dev/moq/blob/main/rs/moq-loc/MIGRATE.md), [moq-msf](https://github.com/moq-dev/moq/blob/main/rs/moq-msf/MIGRATE.md), [moq-mux](https://github.com/moq-dev/moq/blob/main/rs/moq-mux/MIGRATE.md), [moq-net](https://github.com/moq-dev/moq/blob/main/rs/moq-net/MIGRATE.md), [moq-nvenc](https://github.com/moq-dev/moq/blob/main/rs/moq-nvenc/MIGRATE.md), [moq-pattern](https://github.com/moq-dev/moq/blob/main/rs/moq-pattern/MIGRATE.md), [moq-quic](https://github.com/moq-dev/moq/blob/main/rs/moq-quic/MIGRATE.md), [moq-room](https://github.com/moq-dev/moq/blob/main/rs/moq-room/MIGRATE.md), [moq-rtc](https://github.com/moq-dev/moq/blob/main/rs/moq-rtc/MIGRATE.md), [moq-rtmp](https://github.com/moq-dev/moq/blob/main/rs/moq-rtmp/MIGRATE.md), [moq-sock](https://github.com/moq-dev/moq/blob/main/rs/moq-sock/MIGRATE.md), [moq-srt](https://github.com/moq-dev/moq/blob/main/rs/moq-srt/MIGRATE.md), [moq-stats](https://github.com/moq-dev/moq/blob/main/rs/moq-stats/MIGRATE.md), [moq-tokio](https://github.com/moq-dev/moq/blob/main/rs/moq-tokio/MIGRATE.md), [moq-transcode](https://github.com/moq-dev/moq/blob/main/rs/moq-transcode/MIGRATE.md), [moq-uring](https://github.com/moq-dev/moq/blob/main/rs/moq-uring/MIGRATE.md), [moq-v4l](https://github.com/moq-dev/moq/blob/main/rs/moq-v4l/MIGRATE.md), [moq-video](https://github.com/moq-dev/moq/blob/main/rs/moq-video/MIGRATE.md), [qmux](https://github.com/moq-dev/moq/blob/main/rs/qmux/MIGRATE.md)
- **JavaScript:** [`@moq/auth`](https://github.com/moq-dev/moq/blob/main/js/auth/MIGRATE.md), [`@moq/boy`](https://github.com/moq-dev/moq/blob/main/js/moq-boy/MIGRATE.md), [`@moq/clock`](https://github.com/moq-dev/moq/blob/main/js/clock/MIGRATE.md), [`@moq/hang`](https://github.com/moq-dev/moq/blob/main/js/hang/MIGRATE.md), [`@moq/json`](https://github.com/moq-dev/moq/blob/main/js/json/MIGRATE.md), [`@moq/loc`](https://github.com/moq-dev/moq/blob/main/js/loc/MIGRATE.md), [`@moq/msf`](https://github.com/moq-dev/moq/blob/main/js/msf/MIGRATE.md), [`@moq/net`](https://github.com/moq-dev/moq/blob/main/js/net/MIGRATE.md), [`@moq/p2p`](https://github.com/moq-dev/moq/blob/main/js/p2p/MIGRATE.md), [`@moq/pattern`](https://github.com/moq-dev/moq/blob/main/js/pattern/MIGRATE.md), [`@moq/publish`](https://github.com/moq-dev/moq/blob/main/js/publish/MIGRATE.md), [`@moq/qmux`](https://github.com/moq-dev/moq/blob/main/js/qmux/MIGRATE.md), [`@moq/room`](https://github.com/moq-dev/moq/blob/main/js/room/MIGRATE.md), [`@moq/signals`](https://github.com/moq-dev/moq/blob/main/js/signals/MIGRATE.md), [`@moq/ui`](https://github.com/moq-dev/moq/blob/main/js/ui/MIGRATE.md), [`@moq/video`](https://github.com/moq-dev/moq/blob/main/js/video/MIGRATE.md), [`@moq/watch`](https://github.com/moq-dev/moq/blob/main/js/watch/MIGRATE.md)

## Unreleased

These land with the next breaking release.

- **The default priority byte moves again.** Priorities are signed in every
  API, -128 to 127 with 0 unset, so nobody has to know that 127 is the middle
  of a byte. The wire keeps one byte and one urgency on both protocols, so an
  unset priority now goes out as 128 on moq-lite (was 127) and 127 on
  moq-transport (was 128); the previous release moved it to match moxygen.
  hang's media priorities now sit above that default rather than below it.
  Mixed versions only differ in how an unset priority ranks against a set one,
  so upgrade publishers and players in either order.
- **moq-transport drafts 14 through 17 are dropped.** Rust and JavaScript
  speak moq-transport drafts 18 and 22 only, and the `moql` SETUP
  negotiates moq-lite only. A peer that offers nothing newer fails
  to connect instead of falling back, so move such peers to draft 18, draft 22, or a
  moq-lite version before upgrading what they connect to.
- **WebSocket needs a QMux subprotocol** (#6231). Relays answer a WebSocket
  upgrade that offers no `qmux-NN` subprotocol with 400 instead of falling back
  to the legacy `webtransport` format, so upgrade such clients before relays.
- **Data-track breaks are an object with no bytes** (#5815). A hang data
  track (TS private PES, sections, `m2ts`, text) marks a discontinuity with a
  group of one empty object instead of a skipped group. Older consumers fail
  to decode it, so upgrade exporters and players before publishers.
- **Auth requests name `webtransport`** (#5293). Relays report a WebTransport
  session as `webtransport` instead of `quic`, and older auth servers reject the
  new value, so upgrade auth servers before relays.
- **Auth roles are pattern arrays.** Uncapped tokens and grants keep their
  format. An object role, including one with a `rate` cap, is refused; use
  pattern arrays for `publish` and `subscribe`.
- **HLS records replace archive timelines.** The hang catalog's root `archive`
  section is gone. A video or audio rendition carries its own `hls` field
  naming a `<track>.hls` record track, which Rust publishers write by default.
  Neither side reads the other's shape, so upgrade HLS exporters and their
  publishers together.
- **Announce restarts.** moq-lite 07 adds `ANNOUNCE_RESTART` (0x3): another
  publisher instance now serves the prefix. lite-06's `ANNOUNCE_RESTART` is
  named `ANNOUNCE_UPDATE`, as it always meant. moq-transport with the epoch
  extension advertise another epoch before withdrawing the old
  one. Elsewhere, a restart is still an end then a start.
- **Publisher epochs ride the route** (#4942). moq-lite 07 carries the epoch
  after the path in `ANNOUNCE_START`, `TRACK`, `SUBSCRIBE`, and `FETCH`.
  Routes without an epoch no longer resume across routes, so default lite-06
  sessions, including cluster GOAWAY redials, lose seamless cross-route
  failover until lite-07 is the default.
- **Route cost is one value** (#4661). Upgraded nodes write lite-06's cold
  cost at 2^62-1, so an older relay that still ranks on `(warm, cold)` breaks
  equal-warm ties against routes that crossed an upgraded node. It is only a
  tie-break and ends once the fleet is upgraded.
- **A track is timed or untimed, and nothing fills in a timestamp on
  receive.** Tracks from moq-lite before 05, and moq-transport tracks without
  `TIMESCALE` arrive untimed instead of
  stamped with their arrival time. moq-lite 05 and 06 can't encode absence and
  carry a send time instead, so their receivers drop every timestamp and those
  tracks arrive untimed too: read media time from the payload. moq-transport
  timestamps are served to moq-lite untimed; see
  [untimed tracks](/concept/moq-lite#subscriptions). A new subscriber with no
  start on an untimed track starts at the latest group.
- **Max age and max delay count wall-clock time too.** A superseded group is
  stale once its successor arrived a full budget ago, as well as by media
  time, so an untimed track is no longer exempt. With the default zero
  `max_delay`, an untimed subscriber starts at the latest group and skips each
  superseded one; give it a budget to replay or tolerate reordering.

## Released

Older protocol versions still negotiate, so relays and clients can be upgraded
in any order, apart from these:

- The lite 06 ALPN is `moq-lite-06`, not `moq-lite-06-wip`. An explicit
  `moq-lite-06-wip` in a version list is refused (#3941).
- The hang catalog's root `timeline` entry is `archive`, and wall time moved to
  a root `clock: { wall, timescale }` (#3612, #3675). A new `moq export hls`
  finds no timeline in an old publisher's catalog, so upgrade publishers
  before the HLS gateway.
- **Token grants are patterns.** Existing `put`/`get` tokens and key scopes
  keep working as subtrees, and subtree-only grants are still signed in that
  form, so a `moq-token` deployment can upgrade issuers and verifiers in either
  order. Verifiers on the pattern-only `moq-auth` 0.1.0/0.1.1 or `@moq/auth`
  0.1.x/0.2.0 refuse that form, so upgrade them before their issuers. Grants
  only a pattern can express need an upgraded verifier.
- **Gossip discovery is removed.** Until every relay that ran
  `--cluster-mesh` is upgraded, keep client grants off `.internal/`: an older
  relay still dials any URL announced there with `cluster.token`, and an
  upgraded peer still forwards it.
- **Stats counters were renamed.** Relays still write the legacy names beside
  the new ones, so move consumers to the new names now; [Stats](/concept/stats#traffic)
  lists both.
