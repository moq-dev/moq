//! Hang catalog. JSON, served over the `catalog.json` track.
//!
//! [`Producer`] publishes both the hang and MSF tracks together;
//! [`Consumer`] subscribes to the hang track.

mod consumer;
mod producer;

pub use consumer::Consumer;
pub use producer::{Guard, Producer};
