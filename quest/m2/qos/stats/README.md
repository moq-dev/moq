# Client stats

## Goal

Any MoQ client can publish its own stats as a broadcast the way the relay
does: a publisher reports what it sends, a subscriber reports what it received
and played, and a consumer authorized to see them, a dashboard or the publisher
itself, reads them per broadcast. The same `moq-stats` layout carries the
relay's delivery counters and a media client's media counters, so one consumer
reads both, and a broadcast name ending in `.stats` is how a dashboard tells
telemetry from content. Encoders close the loop: a Rust publisher can adapt its
bitrate to what its viewers report. Not here: no wire change, no clock
synchronization, no requirement that any client report, and no relay
involvement beyond routing.

## Plan

Decisions settled while planning:

- **Reuse moq-stats.** `moq_stats::Producer<E>` and `Consumer<E>` take an
  extension flattened next to `Traffic` in every per-broadcast entry, the way
  `hang::timeline::Record<E>` does; `()` is the relay, `hang::Stats` is a
  media client. Old consumers ignore the extra fields; a consumer that knows
  the extension reads both halves. A client's `Traffic` comes from the same
  registry the relay uses, attached through `Client::with_stats`.
- **One broadcast per client at a path its token allows, ending in
  `.stats`**: `pilot/feedback.stats`, `room/alice.stats`. The relay's
  `.stats/node/<node>` prefix convention stays; moq-stats documents both.
  Reporting is opt-in because reading is: a publisher subscribes to a prefix
  it chose, never to every viewer.
- **`publisher.json` and `subscriber.json` stay the everything entry points**,
  keyed by broadcast path as the client sees it, so a transcoder's report
  attributes each rung to its broadcast. A consumer that wants one broadcast
  requests `<path>/subscriber.json` or `<path>/publisher.json` (plus `.z`)
  and the producer serves it on demand, the way tier tracks are served
  today. On a tiered relay the name is `<tier>/<path>/...`, matched against
  the producer's tier labels longest first; a default-tier broadcast whose
  path starts with another tier's label is refused rather than guessed.
- **Counters are cumulative**, like `Traffic`, so `.z` merge-patch deltas stay
  small and the aggregate can sum across clients. Gauges, a latency or a
  target bitrate, are carried but never summed.
- **Publisher transport rides here.** The publisher role carries a
  `transport` section sampled from `ConnectionStats` for the connection the
  broadcast is published over, replacing the connection-scoped channel the
  QoS line planned. Self-reports are diagnostics: never billing,
  authorization, or route-selection input.
- **On dev**, because `moq-stats` is a published crate and the generic
  producer is a breaking change, and the moq-json rework there is what the
  producers build on.

## Quests

- [Schema and library](/quest/m2/qos/stats/schema.md) - moq-stats takes an
  extension, serves per-broadcast tracks, and hang defines the media stats
- [Rust reporters](/quest/m2/qos/stats/rust.md) - the CLI, players, and
  encoders publish and read `.stats` broadcasts
- [Browser reporters](/quest/m2/qos/stats/js.md) - `@moq/stats` mirrors the
  crate, and the browser publisher and player report through it
- [Encoder feedback](/quest/m2/qos/stats/encoder-feedback.md) - a Rust
  encoder subscribes to its viewers' stats and adapts its bitrate

## Closes

- [#3608](https://github.com/moq-dev/moq/issues/3608) - close this issue when the questline finishes
