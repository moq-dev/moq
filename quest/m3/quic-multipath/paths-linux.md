# [L] moq-tokio opens extra paths on Linux, and relays accept them

## Goal

On built binaries: a moq-relay whose `[quic]` config sets a maximum path
count accepts extra paths, on the tokio and io_uring runtimes, and a
moq-tokio client on Linux that set one opens a path per configured
interface, each with a role, from its one UDP socket. A session handle
opens, closes, and re-roles a path at run time and reports each path's
state and stats. `moq publish` from a host with two links keeps publishing
when its sending link is cut, with no redial. On other platforms a client
configured to open an extra path refuses to start.

## Plan

Decisions are in the [epic](/quest/m3/quic-multipath/README.md): mechanism
here and policy in the application, Linux first, opt-in on both ends.

Recommended:

- One socket, with the egress interface chosen per packet through the
  interface index in `IP_PKTINFO` and `IPV6_PKTINFO`, which `moq_sock::udp`
  already encodes as zero. It keeps
  [one socket per relay](/quest/m3/relay-one-socket.md) and needs no
  privilege, where `SO_BINDTODEVICE` needs one. Confirm the kernel honors
  it for the routing lookup before building on it.
- Config nests under `[quic]` with typed fields (an interface and a role
  per entry), not `name=role` strings, per `AGENTS.md`. A setting the
  platform cannot honor stops startup.
- The session handle follows the path lifecycle enum from the
  [protocol quest](/quest/m3/quic-multipath/core.md), and per-path stats
  join the session's stats beside the totals.
- Every path reaches the relay that holds the connection, as the epic's
  Plan lays out. An extra path goes to the address the handshake reached,
  never to a fresh DNS answer. An interface of the other address family
  gets a path only to the relay's `preferred_address` of that family
  (`listen.preferred_v4`, `listen.preferred_v6`); with none advertised it
  gets no path, and its state says why. Cellular links are often IPv6-only,
  so this case is common. Check RFC 10069's text on preferred addresses
  before relying on it.
- Document in `doc/bin/relay/config.md` what each deployment needs: nothing
  on a unicast address, `[listen.lb.forward]` behind an anycast address or a
  VIP, and that an io_uring relay refuses `lb.id` and preferred addresses,
  so it accepts paths only on a unicast address of one family.
- Not in moq-ffi or JS until the
  [Android](/quest/m4/quic-multipath-android.md) and
  [iOS](/quest/m4/quic-multipath-ios.md) quests need it.

Tests in network namespaces, links shaped with bufferbloat: a relay and a
publisher joined by two links; the second path validates; cutting the
sending link leaves the subscriber's gap within a few probe timeouts and
the session open; a backup-role link carries almost nothing until then; a
relay without the setting leaves the client on one path; the same against
an io_uring relay; two relays behind one address with `[listen.lb.forward]`
keep a path that lands on the wrong one; an IPv6-only second link gets its
path through the preferred address. Report the gap.

Public API: additive in moq-tokio, moq-relay's config, and `moq`'s flags.
Wire: none beyond the protocol quest's.

## Hosts

- linux - interface selection and the namespace tests are Linux-only

## Required

- [moq-quic carries one connection over several paths](/quest/m3/quic-multipath/core.md) - the protocol, roles, and path API the driver exposes
