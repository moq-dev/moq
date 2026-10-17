# [L] Compressed tracks in Rust moq-net

## Goal

A Rust publisher sets `track::Info::compress` and moq-net DEFLATEs each
group. A lite-07-wip peer receives the compressed bytes with the flag set;
every other session (lite-01..06, every IETF draft) receives the same frames
inflated, with no flag, from one inflated copy per group shared across all
plain readers. In-process consumers read plain frames. js/net parses the new
lite-07 fields and refuses a compressed track until
[JavaScript](/quest/m1/compress/js.md) lands.

## Plan

See the [epic](/quest/m1/compress/README.md) for the decisions.

- Move the codec from `rs/moq-flate/src/lib.rs` into a crate-private
  `rs/moq-net` module. The crate itself is deleted in
  [Migrate](/quest/m1/compress/migrate.md), with its last users.
- `track::Info` gains a documented `pub compress: bool`, default `false`.
- Producer: writing a frame on a compressed track compresses it through the
  group's encoder and records the plain size on `frame::Info.size`. A group
  keeps one encoder from its first frame to its finish. A frame written in
  chunks (`create_frame`) buffers until it finishes, since the wire needs
  its compressed size first; ingress already knows both sizes from the
  header, so it streams.
- Storage: the model stores the compressed slices. Ingress from a session
  stores what arrived, never recompressing. The shared inflated copy hangs
  off the group, is built on first plain read, is billed to `cache::Pool`,
  and drops with the group. The declared size is checked against the same
  limits as a plain frame's before inflating. A frame whose inflated output differs from its
  declared size, larger or smaller, fails the group, since IETF egress has
  already written that size as the payload length.
- Egress: a lite-07 session writes the compressed slices with the flag in
  TrackInfo and the declared size in the frame header. Any other session
  writes the inflated copy without the flag. A compressed track never
  reaches a peer as compressed bytes it was not told about.
- Wire (lite-07-wip): TrackInfo gains the flag. The frame header gains an
  uncompressed-size varint, present only on compressed tracks. A datagram
  is its own one-frame group (lite allows one datagram per group), so it
  compresses with a fresh window and no delta, and carries the same size
  varint. Update
  `drafts/draft-lcurley-moq-lite.md` and reference
  `draft-lcurley-moq-flate` for the codec; drop that draft's ".z suffix" and
  "no transport signal" text.
- Ranges: a range from frame N of a compressed group is served from N; the
  requester inflates from the 0..N-1 it holds. Refresh's ratio counts the
  stored, compressed bytes.
- js/net: encode and decode the new lite-07 fields so lite-07 interop keeps
  working, and refuse a compressed track with an error. Nothing sets
  `compress` until Migrate, so no first-party track hits the refusal.
- moq-ffi: `MoqTrackInfo` gains `compress`, with the wrappers and
  `doc/lib/*` per the cross-package sync table.
- Docs inline: `doc/concept` sections on track info and moq-lite.

Tests: a compressed track round-trips through a relay to a lite-07 and a
lite-06 subscriber; N plain readers of one group inflate once; a frame that
inflates to more or less than its declared size fails; a compressed
datagram round-trips; an oversized declaration is refused before
inflating; a chunked frame on a compressed track publishes whole; a range from frame N decodes against
a cached prefix. Run `just test interop --all`. Add a bench swept over plain
readers and compressed groups so the inflate cost stays per group, not per
reader.

Public API: `track::Info::compress`, `MoqTrackInfo::compress`. Wire:
lite-07-wip TrackInfo and frame header.
