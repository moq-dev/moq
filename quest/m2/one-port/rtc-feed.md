# [M] WebRTC on the shared socket

## Goal

`moq-rtc`'s server serves WHIP and WHEP media from a socket it is fed rather
than one it binds: datagrams arrive from the demux's WebRTC hook, replies go
out through the shared socket, ICE candidates advertise the shared address,
and every 4-tuple ICE selects is pinned to WebRTC in the demux's flow table.
Binding its own socket keeps working unchanged.

## Plan

Decided 2026-10-05: this is upstream's work, reversing the 2026-09-30
narrowing that left `Mux::feed` to the embedder. `moq-rtc` is an upstream,
generic crate, so every embedder of the one-port demux gets WebRTC without
reaching into its internals. A standalone `moq-rtc` quest was rejected: the
work only matters beside the demux, so it ships in this line.

Decided 2026-10-05: the API is a socket choice in the server config plus a
send trait, not a public `Mux` with a `feed()` method, so the mux stays
crate-private and one code path serves both cases. A sketch, to adjust as
the implementation settles:

```rust
pub trait Transmit: Send + Sync + 'static {
    fn poll_send(&self, cx: &mut Context<'_>, buf: &[u8], dst: SocketAddr) -> Poll<io::Result<usize>>;
}

pub enum Socket {
    Bind(SocketAddr), // today's behavior
    Fed {
        recv: mpsc::Receiver<(Bytes, SocketAddr)>,
        send: Arc<dyn Transmit>,
        advertised: Vec<SocketAddr>,
        pinned: mpsc::Sender<SocketAddr>,
    },
}
```

Build on `moq-ice` (split out in #5702) rather than beside it.
`moq_ice::Socket { udp, locals, inbound }` already separates the advertised
host candidates (`locals`, from `ice_candidates`) from the bound address and
takes routed datagrams on its `Inbound` queue, and the server mux
(`rs/moq-rtc/src/server/mux.rs`) already routes its own `recv_from` loop
into per-session `Inbound`s. What is left is the send side and the mux's
read loop: `moq_ice::Socket`'s `udp: Arc<UdpSocket>` becomes
`Arc<dyn Transmit>` (`Transmit` lives in `moq-ice`, and `UdpSocket`
implements it, so the bound case behaves as today), and a fed mux routes
datagrams from the demux hook instead of reading a bound socket. Adapt the
sketch above to that: `advertised` is `ice_candidates`, and `recv` feeds
the mux's existing router. Address-family mapping follows the real socket,
not the advertisement: a dual-stack socket bound to `[::]` may advertise a
public IPv4 address, so the `Transmit` impl owns the IPv4-mapped conversion
`moq-ice` applies today. Received and pinned addresses use the same
`moq_ice::canonical` form the demux's flow table keys on, or no pin ever
matches.
When ICE selects a pair, the remote address goes out on `pinned`, so the
demux routes that tuple to WebRTC before any SRT test (see the
[questline README](/quest/m2/one-port/README.md)). A pin also has to be
released when its session ends or ICE moves to another pair: either an
unpin message or idle expiry in the flow table, as SRT's pins have.

Things to watch: the server config is `Clone` today and a receiver is not, so
the fed socket may belong in the server's constructor rather than the
config. The fed mux has nothing to bind lazily, and a dropped feed should
fail the server loudly rather than idle.

Tests: a fed mux completes ICE and DTLS with a client through an in-memory
`Transmit`, reports the selected tuple on `pinned`, and releases it when the
session closes. A fed dual-stack socket serves an IPv4 peer. The existing
bound tests keep passing.

## Required

- [UDP demux](/quest/m2/p2p/udp-demux.md) - the WebRTC hook and flow table this feeds from and pins into
