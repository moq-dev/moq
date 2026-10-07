//! Adapt backend poll sessions to moq-net without changing thread affinity.
mod adapter;
pub use adapter::{Error, RecvStream, SendStream, Session};
