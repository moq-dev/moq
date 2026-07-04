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
//! [`Catalog<E>`](hang::Catalog) snapshots. Wrap it with [`Select`] (driven by a
//! [`select::Broadcast`](crate::select::Broadcast)) to narrow the set before
//! handing it to an exporter; both also implement [`Stream`] so they compose
//! either direction.

pub mod hang;
pub mod msf;

mod consumer;
mod format;
mod producer;
mod select;
mod stream;
mod tracks;

pub use consumer::Consumer;
pub use format::*;
pub use producer::{Guard, Producer};
pub use select::Select;
pub use stream::Stream;
pub use tracks::{Audio, AudioTrack, Kind, Rendition, Reserved, Video, VideoTrack};
