# Media stats and viewer feedback

## Goal

A hang publisher can announce a stats track in its catalog, and a publisher
that wants to hear from its viewers can solicit feedback there too. The
publisher's `stats` track is one snapshot of what it sent, per track and for
its connection. Each viewer publishes one `.echo` broadcast carrying a
feedback track for every soliciting publisher it watches: what it received
and played, per track, and its own connection. A dashboard reads both the
same way a publisher does, and a Rust encoder adapts its bitrate to what its
viewers report. Stats and feedback cost nothing on the network unless someone
subscribes. Not here: the relay's `moq-stats` layout, which stays as it is;
clock synchronization; any requirement that a client report; and feedback as
an input to billing, authorization, or route selection.

## Plan

Decided while planning. This supersedes the moq-stats extension design of
[#4145](https://github.com/moq-dev/moq/pull/4145) and revives
[moq#2734](https://github.com/moq-dev/moq/issues/2734) in a reshaped form:

- **Media stats leave moq-stats.** The relay is media-agnostic and keeps
  `Traffic`, `Presence`, and `.stats/node/<node>` unchanged. Media stats are
  hang tracks, discovered through the catalog, so no `Producer<E>`
  extension, `Merge` wrapper, or flattened generic is needed. One layout for
  relay and clients is given up on purpose.
- **Publisher: `stats: { track }` in the catalog.** A root section naming one
  snapshot track: `{ transport, tracks: { <track name>: stats::Track } }`,
  plus container sections flattened in the way `Catalog<E>` flattens
  `ts::Ext`. It is keyed by the catalog's own track names, so nothing repeats
  the catalog's `video`/`audio` nesting; the kind comes from the catalog
  entry. The stats stay off the catalog track, which would otherwise churn
  for every viewer on each interval.
- **Viewer: feedback only when solicited.** Most publishers do not read
  feedback, so a publisher solicits it with a root `feedback: { track }`
  section naming the track it will read; absent means none. The section is an
  object so later fields stay additive. The publisher chooses the name and
  keeps it unique; a viewer refuses a second catalog claiming a name it
  already serves.
- **`.echo` broadcasts.** A viewer publishes one broadcast whose path ends in
  `.echo`, at a path its token allows, with one feedback track per soliciting
  publisher, named as that publisher's catalog asks. It is not a hang
  broadcast and has no catalog, so no player lists it as content, and the
  suffix lets a reader filter at announce time. Paths are arbitrary: where
  viewers publish and which prefix a publisher watches is application policy,
  and the feedback track name, not a path, pairs the two. The name is generic
  so keyframe requests and bandwidth estimates can join later.
- **Feedback track: one snapshot**, `{ transport, tracks: { <publisher track
  name>: feedback::Track } }`. It is keyed by the publisher's track names, so
  the publisher looks up its own tracks directly.
- **One type per role, shared across kinds.**
  - `stats::Track`: sent frames and bytes, keyframes, skipped frames, target
    bitrate.
  - `feedback::Track`: received, decoded, late, decode errors, stalls,
    stalled duration, underruns, newest arrival, latency.
  - A kind's unused fields are omitted.
  - `transport` is one type at both roles: rtt, rate, loss, sample age. A
    browser has only PROBE rtt until
    [#2733](https://github.com/moq-dev/moq/issues/2733)-style counters land.
- **Encoding**: cumulative counters about once a second through
  `moq_json::snapshot`, with the `.z` merge-patch sibling, produced whenever
  stats are enabled. An unsubscribed track never leaves the process, so no
  on-demand track API is needed, which JS lacks. Gauges are carried but
  never summed.
- **On main.** Every change is additive: optional sections on
  `#[non_exhaustive]` catalog types, and new types. The line left the
  [QoS](/quest/m1/qos/README.md) line, which stays on `dev` for the relay's
  moq-stats changes.
- **No `@moq/stats` package.** Media types live in `@moq/hang`, and the
  demo dashboard's relay-stats reader stays where it is.
- Docs stay inline: `doc/concept/hang.md` documents both catalog sections,
  `doc/concept/stats.md` gains a media section beside the relay's, and
  `drafts/draft-lcurley-moq-hang.md` specs the wire.

## Required

- [Schema](/quest/m1/stats/schema.md) - hang defines the `stats` and
  `feedback` catalog sections, their snapshot types, and the draft text
- [Rust reporters](/quest/m1/stats/rust.md) - the CLI, players, encoders, and
  moq-mux remuxes publish stats and feedback
- [Browser reporters](/quest/m1/stats/js.md) - `<moq-publish>` publishes
  stats and `<moq-watch>` publishes feedback
- [Encoder feedback](/quest/m1/stats/encoder-feedback.md) - a Rust encoder
  reads its viewers' feedback and adapts its bitrate

## Related

- [QoS](/quest/m1/qos/README.md) - the relay's delivery counters, the other
  half of a health verdict
