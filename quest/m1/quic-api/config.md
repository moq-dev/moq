# [L] Plain configs and one way to build an endpoint

## Goal

Every moq-quic and driver config is a plain `#[non_exhaustive]` struct with
public fields and a `Default`, nested by role. `Limits` is the only spelling
of windows and stream counts. An endpoint has one constructor and one
`connect`, connections and endpoints are namespaced
(`connection::{Connection, Error, Stats}`, `endpoint::{Endpoint, Config,
Incoming, ConnectError}`, `transport::Config`, `client::Config`,
`server::Config`), QUIC v1 is the only version, and no function takes four or
more arguments.

## Plan

Decided 2026-10-23 (maintainer):

**Config structs.** quinn's `&mut Self` setters inside `Arc`s become plain
structs, like `Limits` and `mux::Config` already are. Nest by role:
`transport::Config { limits, idle_timeout, mtu, ack_frequency, congestion,
datagram, .. }`, with `MtuDiscoveryConfig`, `AckFrequencyConfig`,
`IdleTimeout`, `ValidationTokenConfig`, and `QlogConfig` as nested modules'
`Config`s. A field set to `None` takes the default at construction, and
`set_limits` leaves it unchanged at runtime, so `Limits` means the same thing
in both places. One `max_streams` covers both directions; document that.

**Endpoint.** `Endpoint::new(endpoint::Config, socket)`, where the socket is
anything `Into<Socket>` (a std `UdpSocket` or a boxed `AsyncUdpSocket`) and
the server config is an optional field. `connect(&client::Config, addr,
server_name)` with no stored default. `rebind(socket)`. `client()`,
`server()`, `new_with_abstract_socket`, `rebind_abstract`, `connect_with`,
and `set_default_client_config` go.

**Socket ownership.** The endpoint owns its socket: dropping it or
`Endpoint::close().await` releases the port, and the driver fans
writability out to every connection itself. moq-tokio's backend wraps the
socket in its own `Arc<RwLock<Option<UdpSocket>>>` with a waker fan-out and
`release()` (about 170 lines with its test) only because every connection's
`UdpSender` keeps the socket alive. `Endpoint::new` outside a tokio runtime
returns an error instead of panicking in `tokio::spawn`, so the backend's
`Handle::try_current()` guards go.

**Limits.** `Limits` gains `stream_receive_window` so it really is the one
spelling, and the idle timeout is a validated `Duration`. moq-tokio's
`quic.rs` (`validate_streams`, `validate_windows`, `validate_idle_timeout`
with their own `MAX_VARINT`/`MAX_STREAMS`), moq-uring's driver, and
moq-relay's `limits.rs` round trip all re-derive `Limits::validate`; they call
it instead. Tests in moq-tokio's backend and moq-uring's driver that grep
`format!("{:?}")` for window values read the public fields instead.

**Connection errors.** `connection::Error` gains `HandshakeTimeout` and
carries close reasons as `String` (lossy UTF-8 at decode); moq-uring's
driver, qmux, and moq-tokio's `quic::web` each map it with their own
`from_utf8_lossy`, and qmux tracks `established` only to split `TimedOut`.
moq-quic emits `ConnectionLost(LocallyClosed { code, reason })` once a local
close is staged, so moq-uring's driver and qmux stop stashing and
republishing their own close (maintainer, 2026-10-23).

**Versions.** `DEFAULT_SUPPORTED_VERSIONS: &[u32]` becomes a `Version` enum
with `V1` only; drafts 29 to 34 and their initial salts and retry keys are
deleted (no MoQ peer speaks them).

**Arity.** moq-quic's `Endpoint::handle` (six arguments), `Endpoint::new`,
`connect`, `accept`, and `start_accept` take structs; `Transmit`'s three
`Option`s get the same look.

**Re-exports.** Each driver re-exports what its signatures name: today
`CreditStats`, `Limits`, and `LimitsError` are missing from
`moq_tokio::quic::driver`, and `Limits` lives at `moq_tokio::quic`.

**WebTransport builders.** `quic::web`'s `ClientBuilder`, `ServerBuilder`,
and `Client` (about 400 lines, quinn's `Endpoint::client`/`server` helpers,
connecting to only the first DNS answer, with their own congestion enum) go;
moq-uring's tests, the bench, and moq-tokio's test use the driver's
`connect` instead (maintainer, 2026-10-23).

Out of scope: the congestion controller API, which another quest is
redesigning (maintainer, 2026-10-23); nest whatever it lands in
`transport::Config`.

The Impact bullet says how to migrate in moq-quic, moq-tokio, moq-uring, and qmux; update
moq-tokio's `quic::Config` resolution and moq-relay's limits.

## Required

- [Trim](/quest/m1/quic-api/trim.md) - removes the duplicate limit setters first
