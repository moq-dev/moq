//! Hang catalog.
//!
//! A JSON document listing every track in a broadcast, republished on
//! the `catalog.json` track whenever a track is added, removed, or
//! reconfigured. [`Producer`] writes both the hang and MSF tracks so a
//! single broadcast satisfies subscribers of either format; [`Consumer`]
//! reads the hang track. [`Container`] is the runtime-dispatched
//! wire-format type built from a catalog entry, suitable for use with
//! [`Consumer<C>`](crate::container::Consumer) and
//! [`Producer<C>`](crate::container::Producer).

mod consumer;
mod container;
mod producer;

pub use consumer::Consumer;
pub use container::Container;
pub use producer::{Guard, Producer};
