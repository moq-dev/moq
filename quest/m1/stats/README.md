# Media stats and viewer feedback

## Goal

A hang publisher can announce a stats track in its catalog, and a publisher
that wants to hear from its viewers can solicit feedback there too. The
publisher's `stats` track is one snapshot of what it sent, per rendition and
for its connection. A viewer publishes one `.echo` broadcast per soliciting
catalog it reads, carrying what it received and played, per rendition, and
its own connection. A dashboard reads both the same way a publisher does. Stats and
feedback cost nothing on the network unless someone subscribes. Not here: the
relay's `moq-stats` layout, which stays as it is; clock synchronization; any
requirement that a client report; and feedback as an input to billing,
authorization, or route selection.

## Plan

Decided while planning. This supersedes the moq-stats extension design of
[#4145](https://github.com/moq-dev/moq/pull/4145), revives
[moq#2734](https://github.com/moq-dev/moq/issues/2734) in a reshaped form, and
answers the per-track stats question raised in
[#4496](https://github.com/moq-dev/moq/pull/4496): one snapshot track per
catalog keyed by rendition, not a stats track per rendition and not a sum per
kind.

- **Media stats leave moq-stats.** The relay is media-agnostic and keeps
  `Traffic`, `Presence`, and `.stats/node/<node>` unchanged. Media stats are
  hang tracks, discovered through the catalog, so no `Producer<E>`
  extension, `Merge` wrapper, or flattened generic is needed. One layout for
  relay and clients is given up on purpose.
- **Publisher: `stats: { track }` in the catalog.** A root section naming one
  snapshot track: `{ transport, renditions: { <alias>: stats::Track } }`,
  plus container sections flattened in the way `Catalog<E>` flattens
  `ts::Ext`. The stats stay off the catalog track, which would otherwise churn
  for every viewer on each interval.
- **Keyed by rendition alias** (2026-09-29). Both snapshots key by the
  catalog's rendition keys, which [catalog track
  alias](/quest/m1/catalog-track-alias.md) makes aliases unique within a
  catalog. Nothing repeats the catalog's `video`/`audio` nesting; the kind
  comes from the catalog entry. A viewer reports a rendition that references
  another broadcast to the catalog that lists it, under its alias there. The
  publisher's own snapshot covers only renditions it writes and omits
  referenced ones, whose sender reports them in its own catalog.
  Reason: a track name alone collides once a catalog lists renditions from
  several broadcasts.
- **Viewer: feedback only when solicited.** Most publishers do not read
  feedback, so a publisher solicits it with a root `echo: { path }` section;
  absent means none. The section is an object so later fields stay additive.
  Root key collisions with application extensions are accepted, not guarded.
- **Echo path in the catalog** (2026-09-29). `path` is relative to the
  broadcast serving the catalog and resolves like a rendition's `broadcast`
  (URL-style, so from `room/live`, `viewers` is `room/viewers` and
  `live/viewers` is `room/live/viewers`). A viewer publishes its `.echo`
  broadcast under the resolved prefix at a name the application gives it. The
  application issues tokens to match. Reason: applications control the
  layout and the token rights, and the publisher reads exactly that prefix.
- **One `.echo` broadcast per catalog** (2026-09-29). A viewer announces it
  only after reading the soliciting catalog, and it carries one feedback
  track at a fixed name hang defines. It is not a hang broadcast and has no
  catalog, so no player lists it as content, and the suffix lets a reader
  filter at announce time. The name is generic so keyframe requests and
  bandwidth estimates can join later. Reason: a shared `.echo` serving a
  track per publisher needed accept-any-name serving, claims, refusals, and
  an unclaimed cap to survive name collisions and a publisher subscribing
  before the viewer read the catalog; one broadcast per catalog removes the
  collisions, the race, and the cap with less code, and needs no JS track
  request API.
- **Trust is the token prefix** (2026-09-29). Whoever the application's
  tokens let publish under the echo path may report, and no report is
  authenticated beyond that.
- **Feedback track: one snapshot**, `{ transport, renditions: { <alias>:
  echo::Track } }`, so the publisher looks up its own renditions directly.
- **One type per role, shared across kinds.**
  - `stats::Track`: sent frames and bytes, keyframes, skipped frames, target
    bitrate.
  - `echo::Track`: received, decoded, late, decode errors, stalls,
    stalled duration, underruns, newest arrival, latency.
  - A kind's unused fields are omitted.
  - `transport` is one type at both roles: rtt, rate, loss, sample age. A
    browser has only PROBE rtt until
    [#2733](https://github.com/moq-dev/moq/issues/2733)-style counters land.
- **Encoding**: cumulative counters about once a second through
  `moq_json::snapshot`, with the `.z` merge-patch sibling, produced whenever
  stats are enabled. An unsubscribed stats track never leaves the process.
  Gauges are carried but never summed.
- **On main.** Every change is additive: optional sections on
  `#[non_exhaustive]` catalog types, and new types. The line left the
  [QoS](/quest/m1/qos/README.md) line, which stays on `dev` for the relay's
  moq-stats changes.
- **No `@moq/stats` package.** Media types live in `@moq/hang`, and the
  demo dashboard's relay-stats reader stays where it is.
- Docs stay inline: `doc/concept/hang.md` documents both catalog sections,
  `doc/concept/stats.md` gains a media section beside the relay's, and
  `drafts/draft-lcurley-moq-hang.md` specs the wire.

Decided in the 2026-09-30 audit: a Rust encoder adapting its bitrate to viewer
feedback moved to [encoder feedback](/quest/m2/stats-encoder-feedback.md) (m2),
along with its open questions. This line only publishes and reads the reports.

## Required

- [Catalog track alias](/quest/m1/catalog-track-alias.md) - rendition keys
  become aliases, the key both snapshots use
- [Schema](/quest/m1/stats/schema.md) - hang defines the `stats` and
  `echo` catalog sections, their snapshot types, and the draft text
- [Rust reporters](/quest/m1/stats/rust.md) - the CLI, players, encoders, and
  moq-mux remuxes publish stats and feedback
- [Browser reporters](/quest/m1/stats/js.md) - `<moq-publish>` publishes
  stats and `<moq-watch>` publishes feedback

## Related

- [QoS](/quest/m1/qos/README.md) - the relay's delivery counters, the other
  half of a health verdict
- [Encoder feedback](/quest/m2/stats-encoder-feedback.md) - a Rust encoder
  adapts its bitrate to what its viewers report
