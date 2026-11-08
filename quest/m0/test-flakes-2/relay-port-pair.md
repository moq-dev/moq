# [XS] bench/relay.test.py's stream-transport test gets a port free for TCP and UDP

## Goal

`bench/relay.test.py`'s `PortOwnership.test_a_stream_transport_needs_its_own_listener`
passes while other checks run in parallel on the same host, never failing its
setup with `Address already in use`.

## Plan

Planned 2026-11-08 (maintainer's standing rule that flakes join this epic). It
failed once locally while other sessions ran checks in parallel. None of the 40
most recent failed merge-queue runs (2026-11-05 to 2026-11-08) failed it, so it
is not quarantined.

The test does not bind a fixed port: it binds its TCP listener to port 0
(line 278), then binds UDP to the same number (line 281,
`udp.bind(("127.0.0.1", port))`). TCP and UDP have separate port spaces, so
the OS's free TCP port says nothing about UDP. A parallel check's QUIC client
holding that UDP port fails line 281.

Decided (rejected options in parentheses):

- ✅ Find a number free in both protocols: bind UDP to port 0, then TCP to the
  same number, and draw again while TCP refuses it. Keep the refused sockets
  open until a pair binds, so each draw gets a new number and the search ends.
  This searches for a free resource rather than retrying a timing race: no OS
  call reserves one number in both protocols. (rejected: a private network
  namespace via `unshare`, which needs unprivileged user namespaces that
  `sh/dispatch.sh` already probes for and some hosts deny; a fake `lsof`, which
  stops testing the real `lsof` filters `relay_owns_port` uses; dropping the
  same-number requirement, which is what the relay binds and the check proves.)
- The sibling tests need no change: `test_tcp_without_udp_is_not_owned_as_a_relay_port`
  binds only TCP to port 0, and the relay startup tests go through
  `start_relay`, which already draws again when a port is taken.

Reproduce first, by hand rather than as a committed test: from a helper process,
hold UDP sockets across all but a few ports of the ephemeral range, and watch
line 281 fail. Then show the fixed test passing under the same pressure. Next,
have the helper hold TCP listeners on all but a few ports instead, so TCP
refuses nearly every draw, and show the search redraws until a pair binds.
Each held socket costs a descriptor: the helper holds one per port, and the
test keeps one UDP socket per refused draw. Raise both processes' soft
`RLIMIT_NOFILE` to cover the range plus a few spare (the Linux default hard
limit, 524288, allows it). The helper is a separate process because
`relay_owns_port` checks the test's own PID, so a pressure listener the test
owned at the drawn port plus two would make the negative `tls` assertion fail.
Real collisions are rare, so the search itself needs no cap.

Public API: none. Wire: none.
