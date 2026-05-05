//! Media muxers and demuxers for MoQ.
//!
//! `moq-mux` sits between [`moq_lite`] (the generic pub/sub protocol) and [`hang`]
//! (the media catalog/container format). It exposes five submodules:
//!
//! - [`container`]: the wire-level container abstraction and per-track wrappers —
//!   the [`Container`](container::Container) trait, the [`Hang`](container::Hang) enum
//!   (Legacy or CMAF), the [`Frame`](container::Frame) type, and the generic
//!   [`Consumer`](container::Consumer)/[`Producer`](container::Producer) wrappers that
//!   dispatch to a `Container` implementation.
//! - [`catalog`]: hang catalog publish/subscribe — [`Producer`](catalog::Producer)
//!   manages the hang and MSF catalog tracks, [`Consumer`](catalog::Consumer)
//!   subscribes to incoming catalog updates.
//! - [`import`]: pull external media (fMP4, HLS, raw codec bitstreams, …) into a
//!   moq broadcast — codec demuxers that publish through a
//!   [`catalog::Producer`].
//! - [`export`]: subscribe to a moq broadcast and decode media frames —
//!   [`Muxed`](export::Muxed) merges every track in a broadcast in timestamp order, and
//!   [`Fmp4`](export::Fmp4) re-encodes decoded frames as ISO-BMFF / CMAF fragments.
//! - [`convert`]: republish a broadcast in a different container format
//!   (Legacy ↔ CMAF) without going through an external transcoder.

pub mod catalog;
pub mod container;
pub mod convert;
mod error;
pub mod export;
pub mod import;

pub use error::*;
