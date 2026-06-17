//! Catalog publish/subscribe.
//!
//! The catalog is a JSON document listing every track in a broadcast:
//! its codec, container, dimensions, and any decoder configuration the
//! subscriber needs. Two encodings coexist on every broadcast:
//!
//! - [`hang`] is hang's original shape, served on the `catalog.json` track.
//! - [`msf`] is the IETF-proposed alternative, served on the `catalog` track.
//!
//! Publishing through [`Producer`] writes both tracks together;
//! subscribers pick one based on the broadcast's filename suffix. See
//! [`CatalogFormat`] for the suffix-to-format mapping. The producer is
//! generic over an application extension `E` (see [`hang::CatalogExt`]),
//! defaulting to `()` for media-only catalogs.
//!
//! On the consume side, [`Consumer`] is the unified entry point: it
//! subscribes to whichever catalog track `format` advertises and yields
//! [`Catalog<E>`](hang::Catalog) snapshots. Wrap it with [`Filter`] (hard
//! match on name / codec family) or [`Target`] (soft match picking one
//! rendition per axis) to narrow the set before handing it to an exporter;
//! both also implement [`Stream`] so they compose either direction.

pub mod hang;
pub mod msf;

mod consumer;
mod filter;
mod format;
mod producer;
mod stream;
mod target;
mod tracks;

pub use consumer::Consumer;
pub use filter::{Filter, FilterAudio, FilterVideo};
pub use format::*;
pub use producer::{Guard, Producer};
pub use stream::Stream;
pub use target::{Target, TargetAudio, TargetVideo};
pub use tracks::{AudioTrack, VideoTrack};
