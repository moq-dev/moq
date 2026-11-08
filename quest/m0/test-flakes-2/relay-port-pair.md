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

Reproduce first: hold the UDP port the test's TCP listener will get (for
example, hold UDP sockets across most of the ephemeral range) and watch line
281 fail, then show the fixed test passing under the same pressure. Also hold
TCP listeners across most of the range, so TCP refuses the first draws, and
show the search redraws until a pair binds. Hold the pressure sockets in a
separate process: `relay_owns_port` checks the test's own PID, so a pressure
listener the test owns at the drawn port plus two would make the negative `tls`
assertion fail, and the test's descriptors stay free for the search.

Public API: none. Wire: none.
