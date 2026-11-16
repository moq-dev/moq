# [M] The ietf extension costs nothing on a moq-lite track

## Goal

On a lite-07 session that negotiated the `ietf` extension, object records and
a Group State ride only the Group Streams, FETCH responses, and datagrams of
a track whose TRACK_PROPERTIES Version is nonzero, meaning a moq-transport
publisher made it. A track published over moq-lite is byte-for-byte what it
is on a session without the extension. `setup::Extensions` and
`with_extensions` leave the public API and the `extensions` config tables:
moq-relay turns the extension on for its own links through a crate-private
door, and no other caller can.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Keep the extension; records and Group State ride only tracks with a
  nonzero Version, and `setup::Extensions` leaves the public API and config
  with the relay enabling it internally (rejected: take the extension out of
  the cut until a consumer is named; keep as is). The 2026-11-09 decision to
  keep the extension stands. The maintainer asked what "pay only when used"
  would be: it is the Version the track already carries, which says whether
  its original publisher spoke moq-transport, so no new flag is needed.

Facts found by the audit:

- A record precedes every frame on every relay link, plain media included.
  `lite::ietf::Records::new(self.ctx.ietf, ..)`
  (`rs/moq-net/src/lite/publisher.rs:6575`) keys on the session, not the
  track, and the draft says so: "In a plain group, every record has a `Skip`
  of 0" (`drafts/draft-lcurley-moq-ietf.md`, Object).
  [moq-dev/moq#6058](https://github.com/moq-dev/moq/pull/6058) measured it
  at 2 bytes per frame.
- The per-track fact already exists: `object_records: ietf &&
  track.info().ietf().is_some()` (`rs/moq-net/src/lite/publisher.rs:2556`),
  added by [moq-dev/moq#8843](https://github.com/moq-dev/moq/pull/8843) so a
  plain track can yield. Only the framing ignores it.
- moq-relay forces the extension on for both directions after loading its
  config, whatever the file says (`rs/moq-relay/src/config.rs:339-340`), so
  `[listen.extensions] ietf` and `[connect.extensions] ietf` are settings
  nobody can use. Nothing else sets it.
- `solicit`, the struct's other field, is "moq-transport only" by its own
  doc (`rs/moq-net/src/setup.rs:81-88`). Every caller that sets it false is
  a test or a benchmark (`rs/moq-net/benches/auth.rs`,
  `rs/moq-relay/tests/integration/auth_lifetime.rs`, `rs/moq-ffi/src/test.rs`,
  `rs/libmoq/src/test.rs`). JS has the same one-field `Extensions`
  (`js/net/src/connection/extensions.ts`).
- `rs/moq-net/AGENTS.md`: a moq-transport feature never adds public API. The
  root file: no opt-out flags.

Guidance for the PR:

- Wire: the receiver knows a track's Version from its Track Stream before
  any group arrives, so both sides agree on the framing with no per-stream
  marker. A track with Version 0 has no Group State and no records, so
  `drafts/draft-lcurley-moq-ietf.md` says the extension changes nothing for
  it, and its rules for a plain group shrink to whatever a track with a
  Version still needs. Rename the draft's "Since -00 (in progress)" heading
  while there: no -00 was published, which `drafts/AGENTS.md` forbids.
- API: delete `setup::Extensions`, `Client::with_extensions`,
  `Server::with_extensions`, the `extensions` field on moq-tokio's
  `connect::Config` and `listen::Config`, the relay's two TOML tables
  (`doc/bin/relay/config.md`), and JS `extensions`. Solicit is always
  offered. The relay reaches the `ietf` offer through a hidden door
  (`#[doc(hidden)]` or a moq-relay-only feature), since moq-relay is a
  separate crate; pick whichever keeps it out of rustdoc and the API diff.
  A config file that still names either table is refused with a message
  naming the removal, as other removed keys are.
- Tests and benchmarks that set `solicit = false` to act as a peer without
  it move to a test-support constructor.
- Tests: a plain track over a link with the extension encodes the same bytes
  as over one without; a moq-transport track still crosses two relays with
  its Object IDs, gaps, and properties (the path the 2026-11-09 decision
  kept, which #8843 noted "is not exercised end to end": add that test).
- With [YIELDED deferred](/quest/m2/yield-defer.md), the "never reset with
  YIELDED" rule for such tracks is already gone; if this lands first, leave
  that sentence to it.
- Run `just test interop --all`.

Public API: `setup::Extensions` and both `with_extensions` leave moq-net,
`extensions` leaves moq-tokio's configs and `@moq/net`. Config:
`listen.extensions` and `connect.extensions` are refused. Wire: lite-07-wip
with the `ietf` extension only; a moq-lite track loses its records.

## Related

- [YIELDED deferred](/quest/m2/yield-defer.md) - deletes the extension's yield exception
- [moq-transport time addressing](/quest/m2/ietf-time-addressing.md) - changes what moq-transport state a track carries
