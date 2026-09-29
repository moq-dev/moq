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
  feedback, so a publisher solicits it with a root `echo: { track }`
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
- **An `.echo` broadcast serves tracks on request.** A publisher can see the
  announcement and subscribe before the viewer has read its catalog, and a
  refusal is final, so a static broadcast would lose that feedback. The
  viewer accepts any requested name and writes an empty snapshot at once (all
  zeros: nothing received from that publisher yet), then fills it when a
  watched catalog claims the name. The first claim binds it; a second catalog
  claiming a bound name is refused. A fixed cap on unclaimed names refuses the
  overflow, so stray requests cannot grow a viewer's tracks without bound.
- **Feedback track: one snapshot**, `{ transport, tracks: { <publisher track
  name>: echo::Track } }`. It is keyed by the publisher's track names, so
  the publisher looks up its own tracks directly.
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

Open, to settle before [encoder feedback](/quest/m1/stats/encoder-feedback.md)
starts:

- **Feedback trust.** Every viewer that can publish under the watched prefix
  counts equally, so one viewer can report false stalls and lower quality for
  the rest. Candidates: a trusted reporter prefix, authenticated reports, or
  a bound on each viewer's influence.
- **Feedback name collisions.** Subscriptions to one track name share a
  track, so a publisher that picks, or guesses, a name another publisher
  claimed first on the same viewer reads that publisher's feedback; refusing
  the second claim does not isolate them. Candidates: an unguessable name, or
  binding the track to the soliciting publisher.

## Required

- [Schema](/quest/m1/stats/schema.md) - hang defines the `stats` and
  `echo` catalog sections, their snapshot types, and the draft text
- [Rust reporters](/quest/m1/stats/rust.md) - the CLI, players, encoders, and
  moq-mux remuxes publish stats and feedback
- [JS track requests](/quest/m1/stats/js-requested.md) - `@moq/net` serves a
  broadcast's tracks on request, as Rust's `broadcast.dynamic()` does
- [Browser reporters](/quest/m1/stats/js.md) - `<moq-publish>` publishes
  stats and `<moq-watch>` publishes feedback
- [Encoder feedback](/quest/m1/stats/encoder-feedback.md) - a Rust encoder
  reads its viewers' feedback and adapts its bitrate

## Related

- [QoS](/quest/m1/qos/README.md) - the relay's delivery counters, the other
  half of a health verdict
