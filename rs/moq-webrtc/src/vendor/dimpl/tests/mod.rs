//! dimpl's integration tests, run against the vendored module.

mod client_hello_recovery;
#[path = "dtls12/main.rs"]
mod dtls12;
mod pre_encryption;
mod premature_records;
mod timing;
