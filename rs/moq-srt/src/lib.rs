//! SRT gateway for MoQ, both directions.
//!
//! Runs an [SRT](https://www.haivision.com/products/srt-secure-reliable-transport/)
//! listener and routes each connection by its stream-id `m=` mode against a
//! [`moq_net::OriginProducer`]:
//!
//! - `m=publish` (the default): demux the MPEG-TS the connection carries with
//!   [`moq_mux`] and publish it into the origin as an ordinary broadcast. The
//!   contribution-ingest analogue of `moq-hls`'s import and `moq-rtc`'s WHIP.
//! - `m=request`: re-mux a broadcast from the origin back to MPEG-TS and stream
//!   it to the caller, so a plain SRT player (VLC, ffmpeg) can watch it.
//!
//! The library is one high-level entry point: build a [`Config`] and hand it
//! plus an origin to [`run`]. A relay embeds the gateway by calling
//! `run(cluster.origin.clone(), config)` alongside its own accept loop; the
//! bundled `moq-srt` binary instead serves the origin locally or forwards it to
//! a remote relay (those paths need the `server` feature).
//!
//! Pure Rust: SRT is provided by `srt-tokio`, with no libsrt or ffmpeg
//! dependency.

mod error;
mod listen;
mod ts;

pub use error::{Error, Result};
pub use listen::{Config, run};
