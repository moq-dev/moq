//! RTMP / enhanced-RTMP gateway for MoQ: contribution ingest *and* egress.
//!
//! Runs an [RTMP](https://en.wikipedia.org/wiki/Real-Time_Messaging_Protocol)
//! server (the protocol OBS, ffmpeg, and most hardware encoders speak) and
//! bridges it to MoQ in both directions:
//!
//! - **Publish (ingest)**: a client (OBS, ffmpeg) pushes a stream in; we re-wrap
//!   its audio/video messages as FLV tags, demux them with [`moq_mux`], and
//!   publish the result into a [`moq_net::OriginProducer`] as ordinary MoQ
//!   broadcasts. This is the contribution-ingest analogue of `moq-srt`,
//!   `moq-cli import ... hls`, and `moq-rtc`'s WHIP.
//! - **Play (egress)**: a client (VLC, ffplay, mpv) pulls
//!   `rtmp://host/<app>/<key>`; we subscribe to that broadcast from a
//!   [`moq_net::OriginConsumer`], mux it back to FLV with [`moq_mux`], and stream
//!   the tags down as RTMP. The counterpart to `moq-cli export ... hls`.
//!
//! Both legacy RTMP (H.264 + AAC, plus MP3) and enhanced RTMP (E-RTMP: the HEVC,
//! AV1, VP9, Opus, AC-3, and MP3 FourCC payloads) are supported in each direction,
//! because the codec handling lives entirely in the [`moq_mux`] FLV demuxer/muxer;
//! this crate only translates the RTMP transport. Legacy players that speak only
//! H.264 + AAC will of course reject the E-RTMP codecs on the play path.
//!
//! Two entry points, depending on how much control you need over each request:
//!
//! - **[`run`]**: the unauthenticated convenience. Build a [`Config`] and hand it
//!   plus an origin to [`run`]; it accepts every publisher and player and routes
//!   by prefix + app/key (publishes into the origin, plays out of it). A relay
//!   embeds this with `run(cluster.origin.clone(), config)`.
//! - **[`Server`] / [`Request`]**: bring your own auth. Loop on
//!   [`Server::accept`], inspect [`Request::app`] / [`Request::stream_key`] (treat
//!   the stream key as a token if you like), then match on the [`Request`]: accept
//!   a [`Publish`] into an origin, or accept a [`Play`] out of one, at a path of
//!   your choosing (or reject it). This is how an embedder (e.g. a relay verifying
//!   a JWT and scoping the origin per token) plugs its policy in, with no
//!   callback. It mirrors `moq-native`'s `Server` / `Request`.
//!
//! Beyond the listener, [`Client`] is the *dial-out* (client) role: connect to a
//! remote RTMP server and either [`publish`](Client::publish) a MoQ broadcast to
//! it (restream MoQ out to Twitch / YouTube / another relay) or
//! [`pull`](Client::pull) a remote stream into an origin (ingest a remote RTMP
//! source). It reuses the same FLV <-> moq-mux plumbing; only the RTMP client
//! transport is new.
//!
//! A command-line interface is provided by the `moq-cli` binary, on top of this
//! library.
//!
//! RTMPS (RTMP over TLS) is supported two ways:
//!
//! - **Let the gateway terminate TLS**: set [`Config::tls`] (or call
//!   [`Server::with_tls`]) with a [`rustls::ServerConfig`], and the listener
//!   speaks `rtmps://` with no other change.
//! - **Bring your own transport**: accept the connection and complete the TLS
//!   handshake yourself (any [`Stream`]: a `tokio_rustls` stream, a custom
//!   socket, a test pipe), then hand the established stream to [`accept_stream`].
//!   Useful when an existing TLS terminator, proxy, or non-TCP transport already
//!   owns the socket.
//!
//! Pure Rust: the RTMP handshake, chunk codec, and session state machine come
//! from [`rml_rtmp`], with no librtmp or ffmpeg dependency.

mod dial;
mod error;
mod flv;
mod listen;
mod server;

pub use dial::Client;
pub use error::{Error, Result};
pub use listen::{Config, run};
pub use server::{Conn, Play, Publish, Request, Server, Stream, accept_stream};

/// Re-export of the `rustls` version this crate builds [`Config::tls`] against,
/// so consumers construct a matching [`rustls::ServerConfig`] (a major `rustls`
/// bump is a breaking change). Only available with the `tls` feature.
#[cfg(feature = "tls")]
pub use rustls;
