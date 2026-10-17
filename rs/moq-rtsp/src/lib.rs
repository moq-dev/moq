//! RTSP camera ingest for MoQ.
//!
//! Pulls an IP camera's H.264 or H.265 video and AAC audio over RTSP and publishes it into a
//! [`moq_net::broadcast::Producer`] the caller supplies, without re-encoding. RTP rides the
//! RTSP connection (TCP interleaved), so it crosses NAT and firewalls with the session it
//! already has: run it on the camera's network and the camera needs no inbound port.
//!
//! [`Client::pull`] ingests exactly one session and never reconnects. It returns once the
//! session ends, fails, or its RTP timeline jumps ([`End::Jumped`]). A broadcast name always
//! means the same content, so a restarted timeline is never spliced onto the old broadcast:
//! the caller finishes it and starts a fresh one under a new epoch, with a fresh catalog. Two
//! sessions (say a camera's SD and HD URLs) can publish renditions of one broadcast by
//! sharing its catalog; each shifts onto the catalog's clock.
//!
//! `moq import rtsp` in `moq-cli` wraps this with reconnect and a fresh epoch per run.
//!
//! Pure Rust: RTSP and depacketizing come from [`retina`](https://docs.rs/retina).

#![warn(missing_docs)]

mod client;
mod error;
#[cfg(any(test, feature = "test-support"))]
pub mod fake;

pub use client::{Client, End};
pub use error::{Error, Result};
