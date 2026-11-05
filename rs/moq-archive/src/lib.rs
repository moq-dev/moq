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
//! - `<track>/.info`: the track's properties as JSON, `{"version":4,"priority":0,"timescale":1000}`,
//!   written before its first object. `timescale` is absent on an untimed track. Properties never
//!   change for a track name. The source's max age is not stored: it bounds the live cache, not
//!   the archive.
//! - `<track>/<group>-<frame>-<pts>` on a timed track, `<track>/<group>-<frame>` on an untimed
//!   one: an object, keyed by its first sequence, the index of its first frame within it, and
//!   that frame's timestamp in the track's timescale. Each field is 19 zero-padded decimal digits,
//!   so a sorted listing is content order and answers group and time lookups; there is no index
//!   object. A group may split between frames across objects, which the frame index keeps unique
//!   and ordered even where timestamps repeat or go backward. Every object is written once.
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
//!   Version (i) = 4,
//!   Frame Start (i),      // the first group's first frame index
//!   Entry Count (i),
//!   Entry (..) ...,
//!   Payload (..),         // every frame's bytes, in table order
//! }
//!
//! Entry {
//!   Sequence (i),         // absolute for the first entry, then current - previous - 1
//!   Frame Count (i),      // at least 1
//!   Frame (..) ...,
//! }
//!
//! Frame {
//!   [Timestamp (i)],      // exactly when the track's .info declares a timescale
//!   Length (i),
//! }
//! ```
//!
//! An archive records stream tracks only. A datagram track is live-only, so a broadcast's
//! archive holds none of its datagram tracks.

#![warn(missing_docs)]

pub use object_store;

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

/// Recording format version written into `.info` and the binary envelope.
pub const VERSION: u64 = 4;

/// The oldest recording format version a [`Consumer`] accepts. Version 2 stored `.info`'s
/// priority as its moq-lite byte, and versions 2 and 3 give each object entry a form, which
/// a reader accepts only as a group.
pub(crate) const VERSION_MIN: u64 = 2;

/// Largest sequence, frame index, or timestamp value the format allows (`2^53 - 1`).
pub const ID_MAX: u64 = (1 << 53) - 1;

/// Decimal width of each object key field.
pub(crate) const ID_WIDTH: usize = 19;
