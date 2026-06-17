//! Catalog publish/subscribe.
//!
//! The catalog is a JSON document listing every track in a broadcast:
//! its codec, container, dimensions, and any decoder configuration the
//! subscriber needs. Two encodings coexist on every broadcast:
//!
//! - [`hang`] is hang's original shape, served on the `catalog.json` track.
//! - [`msf`] is the IETF-proposed alternative, served on the `catalog` track.
//!
//! A single [`Producer`] writes both tracks together; subscribers pick one
//! with the format-specific [`hang::Consumer`] or [`msf::Consumer`] based on
//! the broadcast's filename suffix. See [`CatalogFormat`] for the
//! suffix-to-format mapping.

pub mod hang;
pub mod msf;

mod format;
mod producer;

pub use format::*;
pub use producer::{Guard, Producer};
