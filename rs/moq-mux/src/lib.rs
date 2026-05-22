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
//! - [`catalog`]: hang catalog publish/subscribe. [`Producer`](catalog::Producer)
//!   manages the hang and MSF catalog tracks, [`Consumer`](catalog::Consumer)
//!   subscribes to incoming catalog updates.
//! - [`import`]: pull external media (fMP4, HLS, raw codec bitstreams, ...) into a
//!   moq broadcast. Codec demuxers that publish through a
//!   [`catalog::Producer`].
//! - [`export`]: subscribe to a moq broadcast and produce media bytes.
//!   [`Fmp4`](export::Fmp4) yields a single fMP4 / CMAF byte stream (init segment +
//!   moof+mdat fragments) in timestamp order across tracks.
//!
//! Broadcast names use a filename-style suffix ([`catalog::CatalogFormat::extension`])
//! to advertise their catalog format (`.hang`, `.msf`). Consumers call
//! [`catalog::CatalogFormat::detect`] to pick a catalog track.

pub mod catalog;
pub mod container;
mod error;
pub mod export;
pub mod import;
pub mod transform;

pub use error::*;
