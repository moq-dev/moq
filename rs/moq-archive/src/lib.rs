//! Broadcast archives on any [`object_store::ObjectStore`].
//!
//! A [`Producer`] stores every track of one broadcast under a key prefix the host picks, and a
//! [`Consumer`] serves that prefix back as an ordinary `moq_net` broadcast. Callers that need runtime
//! dispatch supply `Arc<dyn ObjectStore>`; the archive API itself stays generic.
//!
//! # Layout
//!
//! A [`Producer`] writes, under its prefix:
//!
//! - `<track>/.info`: the track's properties as JSON, `{"version":2,"priority":0,"timescale":1000}`,
//!   written before its first object. `timescale` is absent on an untimed track. Properties never
//!   change for a track name. The source's max age is not stored: it bounds the live cache, not
//!   the archive.
//! - `<track>/<group>-<frame>`: an object, keyed by its first group's start and the index of its
//!   first frame within that group. Each field is 19 zero-padded decimal digits, so a sorted
//!   listing is content order; on a timed track a group's start is its time in the track's
//!   timescale, so the same listing answers time lookups, and there is no index object. A group
//!   may split between frames across objects, which the frame index keeps unique and ordered.
//!   Every object is written once.
//! - `<track>/end`: how the track's last run ended, `{"finish":{"group":5,"frame":0}}` for a
//!   clean end at that exclusive position or `{"error":52}` for the stream error code. It sorts
//!   after every object key, and DVR pruning never deletes it.
//! - `.end`, empty: the broadcast ended and no track will be added. It is written after every
//!   track's `end`; without it the archive is still growing, or its producer stopped early.
//!
//! `<track>` is the track name percent-encoded as one segment: everything but `A-Z a-z 0-9 _ -`
//! is escaped with uppercase hex, so a name holding `/` stays one segment and none starts with
//! `.`. A reader refuses a segment that is not in this canonical form.
//!
//! An object is a table followed by every payload in order, each table field a QUIC varint:
//!
//! ```text
//! Object {
//!   Version (i) = 2,
//!   Frame Start (i),      // the first group's first frame index
//!   Entry Count (i),
//!   Entry (..) ...,
//!   Payload (..),         // every frame's bytes, in table order
//! }
//!
//! Entry {
//!   Group Start (i),      // absolute for the first entry, then current - previous - 1
//!   Frame Count (i),      // at least 1
//!   Frame (..) ...,
//! }
//!
//! Frame {
//!   [Timestamp (i)],      // absolute, exactly when the track's .info declares a timescale
//!   Length (i),
//! }
//! ```
//!
//! JSON readers ignore members they do not know, so a later writer may add a member a reader can
//! safely skip without a new version; anything that changes what existing bytes mean takes one.
//! A reader accepts exactly [`VERSION`] and refuses any other with [`Error::Version`].
//!
//! An archive records stream tracks only. A datagram track is live-only, so a broadcast's
//! archive holds none of its datagram tracks.

#![warn(missing_docs)]

pub mod consumer;
mod error;
/// The `.info` object holding a track's immutable properties.
pub mod info;
mod layout;
#[cfg(test)]
mod mock;
mod path;
pub mod producer;
/// The object store and key prefix an archive lives under.
mod store;

pub use consumer::Consumer;
pub use error::{Error, Result};
pub use info::Info;
pub use producer::Producer;
pub use store::Store;

/// The archive format version, written into `.info` and every object, and the only one read.
pub const VERSION: u64 = 2;

/// Largest group start, frame index, or timestamp value the format allows (`2^53 - 1`).
pub const ID_MAX: u64 = (1 << 53) - 1;

/// Decimal width of each object key field.
pub(crate) const ID_WIDTH: usize = 19;
