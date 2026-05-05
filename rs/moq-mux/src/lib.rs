//! Media muxers and demuxers for MoQ.
//!
//! `moq-mux` sits between [`moq_lite`] (the generic pub/sub protocol) and [`hang`]
//! (the media catalog/container format). It exposes four submodules, organized by
//! direction:
//!
//! - [`import`]: pull external media (fMP4, HLS, raw codec bitstreams, …) into a
//!   moq broadcast — codec demuxers + a [`CatalogProducer`](import::CatalogProducer)
//!   that publishes both hang-style and MSF-style catalogs.
//! - [`export`]: subscribe to a moq broadcast and decode media frames —
//!   [`Consumer`](export::Consumer) for a single track, [`Muxed`](export::Muxed) to
//!   merge every track in a broadcast in timestamp order, and [`Fmp4`](export::Fmp4)
//!   to re-encode decoded frames as ISO-BMFF / CMAF fragments.
//! - [`container`]: the wire-level container abstraction shared by the other modules
//!   — the [`Container`](container::Container) trait, the unified
//!   [`Hang`](container::Hang) enum (Legacy or CMAF), and the [`Frame`](container::Frame)
//!   type that flows through the import/export pipelines.
//! - [`convert`]: republish a broadcast in a different container format
//!   (Legacy ↔ CMAF) without going through an external transcoder.

pub mod container;
pub mod convert;
mod error;
pub mod export;
pub mod import;

pub use error::*;
