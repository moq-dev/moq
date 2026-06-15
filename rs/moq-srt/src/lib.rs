//! SRT contribution ingest gateway for MoQ.
//!
//! Runs an [SRT](https://www.haivision.com/products/srt-secure-reliable-transport/)
//! listener, demuxes the MPEG-TS each connection carries with [`moq_mux`], and
//! publishes the result into a [`moq_net::OriginProducer`] as ordinary MoQ
//! broadcasts. Whatever serves that origin (a relay, the bundled binary's serve
//! mode) then exposes the ingested stream like any other broadcast. This is the
//! contribution-ingest analogue of `moq-hls`'s import and `moq-rtc`'s WHIP.
//!
//! The library is one high-level entry point: build a [`Config`] and hand it
//! plus an origin to [`run`]. A relay embeds ingest by calling
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
