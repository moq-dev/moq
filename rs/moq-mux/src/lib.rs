//! Media muxers and demuxers for MoQ.
//!
//! `moq-mux` sits between [`moq_net`] (the generic pub/sub protocol) and [`hang`]
//! (the media catalog/container format). It exposes four submodules:
//!
//! - [`container`]: the wire-level container abstraction and per-track wrappers.
//!   The [`Container`](container::Container) trait, the [`Hang`](container::Hang) enum
//!   (Legacy or CMAF), the [`Frame`](container::Frame) type, and the generic
//!   [`Consumer`](container::Consumer)/[`Producer`](container::Producer) wrappers that
//!   dispatch to a `Container` implementation.
//! - [`catalog`]: hang and MSF catalog publish/subscribe.
//!   [`Producer`](catalog::hang::Producer) manages both catalog tracks,
//!   [`Consumer`](catalog::hang::Consumer) subscribes to incoming hang catalog updates,
//!   [`MsfConsumer`](catalog::msf::Consumer) does the same for MSF.
//! - [`codec`]: per-codec parsing, codec-shape transmuxers, and codec-specific importers
//!   (e.g. [`codec::h264::import::Import`]).
//! - [`import`]: pull external media (fMP4, HLS, MKV) into a moq broadcast.
//! - [`export`]: subscribe to a moq broadcast and produce media bytes.
//!   [`Fmp4`](export::Fmp4) yields a single fMP4 / CMAF byte stream (init segment +
//!   moof+mdat fragments) in timestamp order across tracks.
//!
//! Broadcast names use a filename-style suffix ([`CatalogFormat::extension`](catalog::CatalogFormat::extension))
//! to advertise their catalog format (`.hang`, `.msf`). Consumers call
//! [`CatalogFormat::detect`](catalog::CatalogFormat::detect) to pick a catalog track.

pub mod catalog;
pub mod codec;
pub mod container;
mod error;
pub mod export;
pub mod import;

pub use error::*;
