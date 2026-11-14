# [S] moq-quic's server finishes handshakes under heavy loss

## Goal

A moq-quic server completes a handshake that a mature client keeps
retrying under heavy loss or corruption, so a relay does not shed clients on
bad networks that other servers serve. The QUIC Interop Runner's
`handshakeloss` and `handshakecorruption` cases pass with quinn's and
quiche's clients against moq-quic's server, and their `known` entries in
`test/quic-interop/cases.toml` are gone.

## Plan

Found by [the QUIC Interop Runner quest](https://github.com/moq-dev/moq/pull/8708),
which saw two causes in its first matrix:

- **The handshake inactivity bound is shorter than a client's backoff.**
  `handshake_timeout` (`rs/moq-quic/src/config/transport/mod.rs`, 10 s, as
  msquic's `HandshakeIdleTimeoutMs`) abandons a handshake that has heard
  nothing for 10 s. Under the runner's 30% loss, quinn's client backed its
  probes off to gaps of 13.2 s and 12.9 s, and the server logged
  "handshake timed out" three times in one run. Pick a bound that outlasts a
  client's probe backoff without letting half-open handshakes pin server
  state, for example the idle timeout, or a server bound counted from the
  last packet that made progress. Recommendation: the idle timeout (30 s),
  since a half-open handshake already costs no more than an idle connection.
- **The server burns connection IDs it never used.** On a reordered
  NEW_CONNECTION_ID (sequence 4 before 1 to 3), a server still on sequence 0
  switches at once (`update_rem_cid` where `active_seq() == 0` in
  `rs/moq-quic/src/connection/mod.rs`, inherited from quinn), jumping to 4
  and retiring 0 to 3, then retiring 3, 2, and 1 again as they arrive. That
  is legal but spends the client's spare IDs, and quiche's client then
  fails with `InvalidState` and closes with INTERNAL_ERROR. Switch to the
  lowest unused sequence, and retire only the one left.
- Cover both on the seeded sim under handshake loss, then rerun
  `just test quic-interop --peers quinn,quiche --cases handshakeloss,handshakecorruption --roles server`
  until it is stable, and drop the `known` entries.

The public matrix on <https://interop.seemann.io> shows quinn's and quiche's
clients failing these cases against several other servers too, so part of
the flakiness is theirs. A cell that still fails after both fixes needs its
cause shown in the pcap before it gets a `known` entry back.

Public API: none, unless the bound's default changes, which is a documented
behavior change of `handshake_timeout`. Wire: none.

## Related

- [Public interop listing](/quest/m4/quic-interop-listing.md) - these cells are public there
