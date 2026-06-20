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
//! Pure Rust: the RTMP handshake, chunk codec, and session state machine come
//! from [`rml_rtmp`], with no librtmp or ffmpeg dependency.

mod error;
mod flv;
mod listen;
mod server;

pub use error::{Error, Result};
pub use listen::{Config, run};
pub use server::{Request, Server};
