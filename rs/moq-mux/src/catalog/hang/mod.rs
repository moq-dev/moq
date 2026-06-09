//! Hang catalog.
//!
//! A JSON document listing every track in a broadcast, republished on the `catalog.json` track
//! whenever a track is added, removed, or reconfigured. [`Consumer`] reads the hang track; the
//! shared [`Producer`](crate::catalog::Producer) writes it (alongside MSF). [`Container`] is the
//! runtime-dispatched wire-format type built from a catalog entry, suitable for use with
//! [`Consumer<C>`](crate::container::Consumer) and [`Producer<C>`](crate::container::Producer).

mod consumer;
mod container;
mod ext;

pub use consumer::Consumer;
pub use container::Container;
pub use ext::{Catalog, CatalogExt};
