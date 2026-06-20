//! RTMP / enhanced-RTMP contribution ingest gateway for MoQ.
//!
//! Runs an [RTMP](https://en.wikipedia.org/wiki/Real-Time_Messaging_Protocol)
//! server (the protocol OBS, ffmpeg, and most hardware encoders speak), re-wraps
//! each connection's audio/video messages as FLV tags, demuxes them with
//! [`moq_mux`], and publishes the result into a [`moq_net::OriginProducer`] as
//! ordinary MoQ broadcasts. Whatever serves that origin (a relay, the bundled
//! binary's serve mode) then exposes the ingested stream like any other
//! broadcast. This is the contribution-ingest analogue of `moq-srt`, `moq-hls`'s
//! import, and `moq-rtc`'s WHIP.
//!
//! Both legacy RTMP (H.264 + AAC) and enhanced RTMP (E-RTMP: the HEVC, AV1, VP9,
//! Opus, and AC-3 FourCC payloads) are supported, because the codec handling
//! lives entirely in the [`moq_mux`] FLV demuxer; this crate only translates the
//! RTMP transport.
//!
//! Two entry points, depending on how much control you need over each publish:
//!
//! - **[`run`]**: the unauthenticated convenience. Build a [`Config`] and hand it
//!   plus an origin to [`run`]; it accepts every publisher and routes by prefix +
//!   app/key. A relay embeds this with `run(cluster.origin.clone(), config)`.
//! - **[`Server`] / [`Request`]**: bring your own auth. Loop on
//!   [`Server::accept`], inspect [`Request::app`] / [`Request::stream_key`] (treat
//!   the stream key as a token if you like), then [`Request::accept`] the publish
//!   into an origin at a path of your choosing, or [`Request::reject`] it. This is
//!   how an embedder (e.g. a relay verifying a JWT and scoping the origin per
//!   token) plugs its policy in, with no callback. It mirrors `moq-native`'s
//!   `Server` / `Request`.
//!
//! The bundled `moq-rtmp` binary serves the origin locally or forwards it to a
//! remote relay (those paths need the `server` feature).
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

mod error;
mod flv;
mod listen;
mod server;

pub use error::{Error, Result};
pub use listen::{Config, run};
pub use server::{Conn, Request, Server, Stream, accept_stream};

/// Re-export of the `rustls` version this crate builds [`Config::tls`] against,
/// so consumers construct a matching [`rustls::ServerConfig`] (a major `rustls`
/// bump is a breaking change). Only available with the `server` feature.
#[cfg(feature = "server")]
pub use rustls;
