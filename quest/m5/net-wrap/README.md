# Wrapper traits over moq-net

## Goal

A wrapper such as e2ee sits between any moq-net writer or reader and the
network without changing the writer or reader. moq-net exports traits for
its producers and consumers at the broadcast, track, group, and frame levels,
datagrams included, and moq-net's own types implement them. Every writer and
reader above moq-net takes the traits, so moq-mux, moq-json, moq-stats, hang,
and the CLI can be handed an encrypted broadcast or track and publish or
consume it unchanged. Rust and JS use the same names.

Compression is not a wrapper: it moves into moq-net as a track flag
([Compressed tracks](/doc/concept/compression.md)), which deletes
moq-flate, so moq-flate is out of scope.

Non-goals:
- A runtime seam: libraries poll and spawn nothing ([the remaining library spawns](/quest/m4/spawn-audit.md)).
- The transport seam: custom transports implement moq-net's `transport::poll` traits, including both native WebTransport backends.

## Plan

Decided 2026-10-20 (maintainer), replacing the 2026-10-17 design-first plan
and its trait-versus-bridge comparison:

- **A trait, implemented directly.** No bridge comparison, prototype
  benchmark, or design gate. The bridge (#5822) is closed and superseded.
- **Generics, not `dyn`.** Static dispatch keeps the frame hot path free of
  vtable calls, and moq-net's own types are the default type parameter, so
  plaintext callers keep their source and pay nothing. The traits carry no
  `Send` bounds beyond moq-net's own; a caller that spawns adds the bound at its
  spawn site.
- **Mirror moq-net's current API exactly.** The trait methods are the
  methods writers and readers call today, with the same names, arguments,
  and errors: `create_track`, `reserve_track`, `requests`, `demand`,
  `create_group(group::Info)`, `append_group`, `write_frame`,
  `create_frame`, `write`, `finish`, `abort(self, Error)`, `closed`,
  `track(name)` then `.subscribe(..)`, `recv_group`, `read_frame`,
  `next_frame`, `read_chunk`,
  the broadcast consumer's `tracks` listing (maintainer, 2026-10-24), and
  their `poll_*` forms. A wrapper adapts
  to moq-net, not the other way around: `moq_crypto::e2ee::Error` converts into
  `moq_net::Error`.
- **Datagrams follow the datagram types.** Datagram tracks have their own
  producer and consumer types (`datagram::Producer::append` and `insert`,
  `datagram::Consumer::poll_info` and `poll_recv`), so the traits mirror
  those types rather than stream tracks.
- **Terminal propagation crosses the wrapper.** Finish, typed aborts (such
  as e2ee's authentication error), and handle teardown reach the other side
  at every level, rather than collapsing into `Error::Dropped` or keeping a
  producer alive.
- **Demand and preference cross the wrapper.** moq-mux and the browser
  publisher gate output on `track::Demand`, so a wrapper must not keep
  encoders active with no downstream viewer or drop downstream preference
  updates. Test idle and active demand through a wrapper.
- **Scope is every writer and reader above moq-net:** moq-mux (with the
  moq-video and moq-audio encoders built on it), moq-json, moq-stats, hang,
  and the CLI paths, plus the JS packages that need it.
- **Ships in m5 with e2ee**, not in m2's breaking release: it is a public
  API break, and e2ee is its first consumer.

Today moq-net has no model traits (only `Consume<T>` in
`rs/moq-net/src/model/origin.rs`). moq-mux and moq-json take concrete types,
for example `container::Producer::new(track::Producer)` and moq-json's
snapshot, stream, and window `Producer::new(track::Producer)` and
`Consumer::new(track::Subscriber)`.

Open: the trait names. Bring options with a recommendation in the
[moq-net traits](/quest/m5/net-wrap/traits.md) PR; the methods are fixed
above.

## Related

- [E2EE](/quest/m5/e2ee/README.md) - the first wrapper, which publishes and consumes through these traits
