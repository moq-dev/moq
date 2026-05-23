//! Media muxers and demuxers for MoQ.
//!
//! `moq-mux` sits between [`moq_net`] (the generic pub/sub protocol) and [`hang`]
//! (the media catalog/container format). It exposes:
//!
//! - [`container`] — wire-level container abstraction (the [`Container`] trait,
//!   the [`Hang`] dispatcher, the per-format submodules [`fmp4`], [`mkv`],
//!   [`legacy`], [`loc`], [`hls`]). External file containers (fmp4, mkv, hls)
//!   ship with `import::Import` / `export::Export` submodules.
//! - [`codec`] — per-codec parsing, codec-shape transmuxers, and codec-specific
//!   importers (e.g. [`codec::h264::import::Import`]).
//! - [`catalog`] — hang and MSF catalog publish/subscribe.
//!   [`catalog::hang::Producer`] manages both catalog tracks;
//!   [`catalog::hang::Consumer`] and [`catalog::msf::Consumer`] subscribe.
//! - [`import`] — [`Framed`](import::Framed) / [`Stream`](import::Stream)
//!   dispatchers that pick the right concrete importer from a user-supplied
//!   format string.
//!
//! [`Container`]: container::Container
//! [`Hang`]: container::Hang
//! [`fmp4`]: container::fmp4
//! [`mkv`]: container::mkv
//! [`legacy`]: container::legacy
//! [`loc`]: container::loc
//! [`hls`]: container::hls
//!
//! Broadcast names use a filename-style suffix
//! ([`CatalogFormat::extension`](catalog::CatalogFormat::extension)) to
//! advertise their catalog format (`.hang`, `.msf`). Consumers call
//! [`CatalogFormat::detect`](catalog::CatalogFormat::detect) to pick a catalog track.

pub mod catalog;
pub mod codec;
pub mod container;
mod error;
pub mod import;

pub use error::*;
