//! Hang catalog: JSON-encoded broadcast description served over a moq-net track.
//!
//! [`Producer`] manages publishing (both the hang and MSF catalog tracks);
//! [`Consumer`] subscribes to the hang catalog and decodes updates.

mod consumer;
mod producer;

pub use consumer::Consumer;
pub use producer::{Guard, Producer};
