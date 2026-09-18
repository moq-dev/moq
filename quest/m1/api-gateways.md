# [M] The gateway crates share moq-net's types and errors

## Goal

`moq-rtmp`, `moq-srt`, `moq-rtc`, `moq-hls`, `moq-stats`, and `moq-room`
release with typed errors an embedder can map, paths and durations in
moq-net's types, and constructors that take only what they use. Every one
of these crates is already breaking on dev, so the ride-along costs nothing
now and a full bump later.

## Plan

- `moq_rtc::Server::new(config)`; the origin pair moves to
  `publish_router(origin)`/`subscribe_router(origin)`, the only place it is
  read. moq.pro spawns an unused origin driver to satisfy the constructor.
- Delete `Error::Other(anyhow)` from all four gateway crates. moq-rtc wraps
  `Unauthorized` and "not announced" as `Other`, so moq.pro answers WHIP
  refusals with 500 and WHEP internal failures with 404; route them through
  `Error::Moq` and add the few typed variants missing (`CatalogTimeout`,
  `NoRenditions`, an RTMP `Session(String)`). moq-hls keeps `reqwest` and
  `url` errors opaque and drops the two `pub use`s, as #3243 did elsewhere.
- `listen::Config.prefix` is a `PathOwned` on moq-rtmp and moq-srt (a
  `String` that needs a trailing slash today); moq-stats already does this.
- `moq_hls::Segment.duration: Duration` (an `f64` of seconds built from a
  `Duration`); `Kind` parses through a real error or `Kind::parse ->
  Option`; `import::Config.playlist` is a `Url` (or a `Playlist` enum)
  instead of a string parsed at run time.
- moq-stats: `produce::Config`, `consume::Config`, `consume::{Traffic,
  Sessions}` replace the root `ProducerConfig`/`ConsumerConfig`/
  `TrafficConsumer`/`SessionsConsumer`, matching `aggregate::Config`.
- `moq_room::claims(room: impl AsPath, identity: impl AsPath)`; the room is
  documented as a path prefix and taken as a `String`.
- `moq_srt::{Publish, Subscribe}::reject(self, Reject)` with
  `Reject::{Unauthorized, Forbidden, Unavailable, BadRequest}` mapped to the
  SRT extended codes (1401, 1403, 1503, 1400); today `reject()` sends a
  fixed `Forbidden`. A `moq_srt` enum rather than a re-export of
  `srt_tokio::ServerRejectReason`, so a backend swap is not an API
  migration. Prove it over the wire for a rejected publish and a rejected
  subscribe with a non-default reason, beside the existing clean-close
  coverage, and update `rs/moq-cli/src/srt.rs` and the `rs/moq-srt/README.md`
  embedder example.
- `moq_rtmp::Play::with_max_age(Duration)` and
  `Publish::with_max_age(Option<Duration>)` take the same shape.
- Decide the moq-srt dial shape: free functions over a `Config` today where
  moq-rtmp has a `Client` builder with the bring-your-own-transport seam
  moq.pro uses. Recommended: the `Client`.

Public API: breaking on every crate named, so on dev. Wire: an SRT
client sees the extended reject code that matches the verdict; run
`just test smoke-full`. Consumers: moq-cli, moq.pro's edge (all four
gateways in-process).

## Related

- [Gateway embedding](/quest/m2/gateway-embed.md) - the additive entry points an in-process embedder still lacks
