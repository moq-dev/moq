//! SRT gateway for MoQ, both directions.
//!
//! Runs an [SRT](https://www.haivision.com/products/srt-secure-reliable-transport/)
//! listener and routes each connection by its stream-id `m=` mode against a
//! [`moq_net::origin::Producer`]:
//!
//! - `m=publish` (the default): demux the MPEG-TS the connection carries with
//!   [`moq_mux`] and publish it into the origin as an ordinary broadcast. The
//!   contribution-ingest analogue of `moq-cli import ... hls` and `moq-rtc`'s WHIP.
//! - `m=request`: re-mux a broadcast from the origin back to MPEG-TS and stream
//!   it to the caller, so a plain SRT player (VLC, ffmpeg) can watch it.
//!
//! Two entry points, depending on how much control you need over each request:
//!
//! - [`run`]: the unauthenticated convenience. Build a [`Config`] and hand it
//!   plus an origin to [`run`]; it accepts every publisher and subscriber and
//!   routes by prefix + resource name. A relay embeds this with
//!   `run(cluster.origin.clone(), config)`.
//! - [`Server`] / [`Request`]: bring your own auth. Loop on [`Server::accept`],
//!   inspect [`Request::resource`] / [`Request::stream_id`] (treat the stream id
//!   as a token if you like), then match on the [`Request`]: accept a [`Publish`]
//!   into an origin, or accept a [`Subscribe`] out of one, at a path of your
//!   choosing (or reject it). This is how an embedder (e.g. a relay verifying a
//!   JWT and scoping the origin per token) plugs its policy in. It mirrors
//!   `moq-native`'s `Server` / `Request`.
//!
//! Beyond the listener, the [`dial`] module is the *dial-out* (client) role:
//! connect to a remote SRT listener as a caller and either [`dial::publish`] a MoQ
//! broadcast to it (restream MoQ out to a remote SRT ingest) or [`dial::pull`] a
//! remote stream into an origin (ingest a remote SRT source). It reuses the same
//! MPEG-TS <-> moq bridge; only the SRT caller transport is new.
//!
//! A command-line interface is provided by the `moq-cli` binary, on top of this
//! library.
//!
//! Pure Rust: SRT is provided by `srt-tokio`, with no libsrt or ffmpeg
//! dependency.

#![warn(missing_docs)]

pub mod dial;
mod error;
mod listen;
mod server;
mod ts;

pub use error::{Error, Result};
pub use listen::{Config, run};
pub use server::{Publish, Request, Server, Subscribe};
