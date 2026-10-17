//! Every integration test, linked into one binary.
//!
//! Each `tests/*.rs` file is its own crate that links this package and its
//! dependencies again, so add new integration tests here as a module.

mod alpn;
mod backend;
mod broadcast;
mod close_code;
mod max_age;
mod receive_credit;
mod reconnect;
mod stream_code;
mod subscription_end_integrity;
mod unannounce;
mod worker;
