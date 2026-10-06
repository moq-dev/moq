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

The mux and each session hold `Arc<dyn Transmit>` instead of
`Arc<UdpSocket>`, and `UdpSocket` implements `Transmit`, so the bound case
behaves as today. Candidates come from `advertised` instead of the bound
socket's local address; it is a list, like `ice_candidates`, so a dual-stack
peer still gets a same-family candidate. Address-family mapping follows the
real socket, not the advertisement: a dual-stack socket bound to `[::]` may
advertise a public IPv4 address, so the `Transmit` impl owns the IPv4-mapped
conversion the session applies today. Received and pinned addresses use the
same canonical form the demux's flow table keys on, or no pin ever matches.
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

- [UDP demux](/quest/m2/one-port/udp-demux.md) - the WebRTC hook and flow table this feeds from and pins into
