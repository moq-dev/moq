# [M] Multipath spike

## Goal

A verdict on whether bonded contribution over multipath QUIC is worth
building, backed by two paths actually carrying one session. Bonding means
sending a broadcast over several links at once (cellular plus cellular plus
wifi) so a field encoder survives any one of them degrading, which is the
business LiveU and Peplink built on proprietary tech and SRT has no answer
for.

## Plan

The protocol work is done and unused. `noq` implements draft-ietf-quic-multipath
in full (`open_path`, `path_events`, `path_stats`, a `Path` type, and a
substantial test suite), but `apply_transport` never calls
`max_concurrent_multipath_paths`, whose default is `None`, so multipath is not
negotiated on any MoQ connection.

The spike is small: call that setter, add a max-paths knob beside the other
transport settings, and open a second path from the connect side. It rides
entirely below the `web_transport_trait` line, because
`web_transport_noq::Session` derefs to `noq::Connection` and the trait requires
`Clone`, so the backend can hand one clone to moq-net and keep another to
drive paths on. No trait change, no moq-net change, and none belongs there: a
browser WebTransport backend could never implement it.

**What the spike is really for is the constraint nobody has written down.**
`max_concurrent_multipath_paths` maps to the `initial_max_path_id` transport
parameter, which both peers negotiate, so a multipath session needs noq on
*both* ends. noq is not the default backend on either branch
(`default = ["quinn", ...]`), and no other backend has the extension: quinn
0.11 lacks it, quiche's `probe_path`/`migrate` is RFC 9000 single-path
migration, and iroh's path logic is its own. So bonded contribution is not an
opt-in client feature; it constrains the relay too. Establish what that
actually costs before anyone builds a fan-out socket.

Prove it on a multi-homed host, which is enough to negotiate and open a second
path and is not yet phone bonding.

Then record what the rest would take, so a go verdict has something to plan
from and a no-go says why:

- **One endpoint is one UDP socket.** `FourTuple` carries `local_ip` only,
  because "we can only specify the `src_ip`, not the source port", and setting
  a source IP does not choose an egress interface. Android needs
  `Network.bindSocket` and iOS needs an interface bind to reach the cellular
  radio at all. The escape hatch is `Endpoint::new_with_abstract_socket` with a
  fan-out `AsyncUdpSocket` owning one interface-bound socket per radio, which
  then has to answer for `local_addr`, and take the conservative minimum for
  `max_receive_segments` and `may_fragment`.
- **Stats are single-path shaped.** `ConnectionStats` carries one `rtt` and one
  `estimated_send_rate`, and per-path scheduling for contribution would need
  both that and `web_transport_trait::Stats` reshaped.
- **Scheduling policy is unchosen**: duplicate every path for redundancy, or
  stripe across them for capacity.

Naming trap: `Path` in moq-net is the broadcast namespace path, and
`Client::with_path` is the MoQ SETUP resource path. A network path needs a
different name.

Target `dev`, where the crate is `moq-tokio`; the rename has not reached
`main`.

## Related

- [Choose the QUIC parent](/quest/m2/quic/parent.md) - noq's multipath support
  informs the parent choice, but this spike does not block it

## Closes

- [#2276](https://github.com/moq-dev/moq/issues/2276) - close this issue when the quest finishes
