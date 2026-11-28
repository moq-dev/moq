# [XL] moq-quic carries one connection over several paths

## Goal

Two moq-quic endpoints that each set a maximum path count negotiate
[RFC 10069](https://www.rfc-editor.org/rfc/rfc10069), and the client opens,
validates, and abandons paths on a live connection. Each path has its own
packet numbers, acknowledgments, loss recovery, congestion controller, MTU
discovery, and connection IDs, and a role, available or backup, that the
peer learns. Traffic rides the best available path and moves to another
when that path stops answering, so a transfer survives a silent cut of its
path with no new handshake; a backup path carries traffic only while no
available path is alive. A connection that did not negotiate multipath
sends the same bytes as today.

This is the sans-IO crate and its simulator only. Drivers, config, and
built binaries are
[paths on Linux](/quest/m3/quic-multipath/paths-linux.md); sending on
several paths at once is [striping](/quest/m3/quic-multipath/stripe.md).

## Plan

Decisions are in the [epic](/quest/m3/quic-multipath/README.md): opt-in on
both ends, off by default, RFC 10069's final codepoints, and a port of
noq-proto's implementation by hand.

Recommended:

- API in the crate's `poll_*` style: open a path from a local handle to a
  remote address with a role, change its role, abandon it, wait on path
  events, and read stats per path. `Stats::path` becomes one entry per path.
- A path's lifecycle is one enum whose variants carry their data
  (validating, live, abandoned with its reason), per `REVIEW.md`.
- `Transmit` names the path's local end with an opaque handle the driver
  maps to a socket and an interface. Linux can pick the interface per packet
  on one socket, while Android binds a socket per network, so the core must
  not assume either.
- Moving off a path: the first probe timeout on the sending path also sends
  on another live path, and the scheduler prefers a path that is answering.
  A path is abandoned after its own idle timeout. No timer beyond the ones
  loss detection already runs.
- Bound what a peer can make this end hold by the local maximum path count:
  paths, connection IDs per path, and per-path recovery state all scale with
  it and with nothing the peer sends.
- A peer without the extension ignores the transport parameter, and the
  connection runs single-path.
- Connection IDs for every path come from the endpoint's one generator, so
  each carries the worker steering byte and QUIC-LB server ID that route a
  packet from a new address to the connection's owner.

Verify it two ways:

- On the seeded sim with mocked time and bufferbloat-shaped links: a second
  path opens and validates; a silent cut of the sending path moves the
  transfer within a few probe timeouts; a backup path stays idle while an
  available one lives; an abandoned path's state is freed; a peer that
  exceeds the path limit is refused. A property test over random schedules
  of opens, abandons, cuts, and reordering checks every stream arrives once
  and in order, the connection never deadlocks, and memory stays within the
  bound.
- Against another implementation of RFC 10069: add a multipath case to
  `test/quic-interop` with a pinned peer that implements it (picoquic or
  noq). Extend the frame and connection fuzz targets to the new frames.

Measure the single-path cost before and after with `benches/streams`,
`receive`, and `sent_packets`: with multipath off, and negotiated with one
path. Report both; a regression with it off is a bug.

Public API: additive in moq-quic. Wire: a QUIC transport extension,
negotiated by transport parameter and absent unless both ends opt in; no
moq-lite change.

## Related

- Fix dual-stack NAT rebinding, prev_path rollback, and probe gating ([moq-dev/moq#9712](https://github.com/moq-dev/moq/pull/9712)) - edits the same path code; land it first or rebase over it
- [Session keep-alive with a stale signal](https://github.com/moq-dev/moq/pull/9698) - its Stale signal becomes per path
