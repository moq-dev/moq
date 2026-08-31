# [L] moq-native: enable QUIC multipath for bonded contribution (noq already implements it)

## Goal

Implement and verify the behavior tracked in [#2276](https://github.com/moq-dev/moq/issues/2276)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Bonded contribution: send one broadcast over several network paths at once (cellular + cellular + wifi) so a field encoder survives any one link degrading. LiveU / Peplink / Zixi built an industry on proprietary bonding, and SRT has no native answer. Multipath QUIC is on the standards track, so MoQ could make this a config flag in an open protocol.

I checked how hard it is to hook up. **Short version: noq already implements draft-ietf-quic-multipath in full, and moq never turns it on.** The protocol work is done. The blocker is the single-socket endpoint, which is exactly the thing cellular+wifi bonding needs.

#### What exists today

`noq` (the default backend: `default = ["noq", "aws-lc-rs", "websocket", "tcp", "uds"]` in `rs/moq-native/Cargo.toml`, via `web-transport-noq` 0.2.0) ships the whole multipath API:

- `noq::Connection::open_path(FourTuple, PathStatus) -> OpenPath` (`noq-1.0.0/src/connection.rs:446`), `open_path_ensure` (`:391`)
- `path_events() -> PathEvents` (`:484`), `path_stats(PathId) -> Option<PathStats>` (`:731`), `is_multipath_enabled()` (`:850`)
- a full `Path` type (`noq-1.0.0/src/path.rs`) with `remote_address()`, `local_ip()`, `set_status`, `close`
- negotiation knob: `TransportConfig::max_concurrent_multipath_paths(u32)` (`noq-proto-1.0.1/src/config/transport.rs:396`), which maps to the `initial_max_path_id` transport parameter. It's `NonZeroU32::new(max_concurrent)` on an `Option`, so the default is **None = disabled**. There's a substantial multipath test suite in `noq-1.0.0/src/tests.rs`.

`apply_transport` (`rs/moq-native/src/noq.rs:15`) sets idle timeout, keep-alive, MTU, stream limits and GSO, and nothing else. So multipath is not negotiated on any moq connection today.

There's already one acknowledgement of the upgrade in-tree, `rs/moq-native/src/noq.rs:469`:

> `// The established Connection no longer exposes a single peer address (noq 1.0 supports multipath), so capture it from the Connecting before awaiting.`

Other backends have nothing: quinn 0.11 has no multipath; quiche 0.29's `probe_path`/`migrate` is RFC 9000 migration (one active path), not the extension; iroh has its own path logic but not this.

#### The layering is already right

`web_transport_trait::Session` is deliberately address-free (open/accept streams, datagrams, stats, close), and `moq_net::Session` erases to `Box<dyn SessionInner>` with three methods (`rs/moq-net/src/session.rs:203`). Neither can see a path, and neither should  -  a browser WebTransport backend could never implement it.

But `web_transport_noq::Session` implements `Deref<Target = noq::Connection>`, and `moq_net::Client::connect<S>(session: S)` takes the session **by value** while the trait requires `Clone`. So `rs/moq-native/src/noq.rs` can clone the session, hand one to moq-net, and keep a live `noq::Connection` to drive paths on. **Multipath rides entirely below the `web_transport_trait` line, invisible to moq-net.** No trait change, no moq-net change.

#### Step 1: the cheap spike (multi-homed host)

Plausibly ~200 lines, works today, no new concepts:

1. `apply_transport` (`rs/moq-native/src/noq.rs:15`) → call `transport.max_concurrent_multipath_paths(n)`.
2. A `--client-quic-max-paths` knob on `quic::Client` (`rs/moq-native/src/quic.rs`  -  note that file is clap/serde config only, not an abstraction).
3. In `NoqClient::connect` (`rs/moq-native/src/noq.rs:172+`), clone the session and `conn.open_path(FourTuple::new(remote, Some(local_ip)), PathStatus::Available)`.

That gets multipath negotiated and a second path open on a multi-homed box. It is **not** phone bonding yet.

#### Step 2: the actual blocker

**One endpoint = one UDP socket.** `bind::udp(addr) -> io::Result<UdpSocket>` (`rs/moq-native/src/bind.rs:26`) is singular, and `noq::Endpoint`'s own doc says "An endpoint corresponds to a single UDP socket". Every backend does this (`noq.rs:150`, `quinn.rs:149`, `quiche.rs:197`).

`FourTuple` is `{ remote: SocketAddr, local_ip: Option<IpAddr> }` and is explicit about why (`noq-proto-1.0.1/src/lib.rs:371`):

> "The socket is irrelevant for our intents and purposes: When we send, we can only specify the `src_ip`, not the source port."

The only send-side lever is `Transmit.src_ip` (i.e. `IP_PKTINFO` on one wildcard socket). **Setting a source IP does not choose an egress interface.** On Linux the routing table might cooperate; on Android you must call `Network.bindSocket()` and on iOS you must bind to the interface to get packets onto the cellular radio at all. One socket cannot be bound to two networks. So cellular+wifi bonding is blocked by the socket model, not by the protocol.

**The escape hatch** is `noq::Endpoint::new_with_abstract_socket(config, server_config, socket: Box<dyn AsyncUdpSocket>, runtime)` (`noq-1.0.0/src/endpoint.rs:162`), documented for exactly this ("useful when `socket` has additional state attached"). `AsyncUdpSocket` is a public trait. Implement a **fan-out socket** owning N interface-bound sockets, dispatching each `Transmit` by `src_ip` and merging the receive queues; noq sees one logical socket, the OS sees one per radio.

Non-trivial: `local_addr()` has to return something coherent, `max_receive_segments()`/`may_fragment()` must be the conservative min across sockets, and interface binding is platform-specific (`SO_BINDTODEVICE`, Android `Network.bindSocket` via JNI  -  moq-native already has a JNI dep and `tls::init_android` doing this kind of reach-through).

#### Also worth noting

`ConnectionStats` (`rs/moq-net/src/session.rs:15`) is single-path shaped: one `rtt`, one `estimated_send_rate`. If per-path scheduling is ever to inform the publisher (and for contribution it should), that struct and `web_transport_trait::Stats` need a per-path story. That's probably the second real design problem after socket binding.

**Naming trap**: `Path` is taken in moq-net (`rs/moq-net/src/path.rs`) and means the broadcast namespace path. Likewise `Client::with_path()` is the MoQ SETUP resource path. A network-path concept needs a different name.

#### Branch

`main`. It's additive and lives entirely in `rs/moq-native` behind `#[cfg(feature = "noq")]`. **Do not put paths in `web_transport_trait` or `moq-net`**  -  it's noq-only and must stay below the trait line.

#### Suggested split

- \[ ] Step 1 spike: negotiate multipath + a `--client-quic-max-paths` knob + `open_path` on a multi-homed host
- \[ ] `AsyncUdpSocket` fan-out over N interface-bound sockets
- \[ ] Platform interface binding (Linux `SO_BINDTODEVICE`, Android `Network.bindSocket`, iOS)
- \[ ] Per-path stats (needs a `ConnectionStats` / `web_transport_trait::Stats` shape decision)
- \[ ] Path scheduling policy for contribution (duplicate for redundancy vs. stripe for capacity)

## Required

- [Plan: QUIC multipath for bonded contribution](/quest/m2/plan-multipath.md) - split into implementable quests first

## Closes

- [#2276](https://github.com/moq-dev/moq/issues/2276) - close this issue when the quest finishes
