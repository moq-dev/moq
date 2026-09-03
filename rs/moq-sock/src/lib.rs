//! Socket and thread-per-core listener plumbing shared by the native MoQ
//! runtimes.
//!
//! [`bind`] opens dual-stack UDP/TCP sockets with sane buffers; [`shard`]
//! forms and steers an `SO_REUSEPORT` group by QUIC connection id, so a
//! worker-per-core listener keeps each connection on the socket that owns it;
//! [`cpu`] pins those workers. Both `moq-tokio` and `moq-uring` build their
//! worker groups on this crate, so the group-formation invariants live here
//! once.

pub mod bind;
pub mod cpu;
pub mod shard;
