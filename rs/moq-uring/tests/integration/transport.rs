#[cfg(all(target_os = "linux", feature = "quic"))]
use moq_uring::quic;

// Exercise the transport adapter on native hosts without a browser or ring.
#[path = "../../src/transport/adapter.rs"]
mod adapter;
