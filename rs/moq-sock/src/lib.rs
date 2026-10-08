//! Socket and thread-per-core listener plumbing shared by the native MoQ
//! runtimes.
//!
//! [`bind`] opens dual-stack UDP/TCP sockets with sane buffers;
//! [`shard::Group`] forms and steers an `SO_REUSEPORT` group by QUIC connection
//! id, so a worker-per-core listener keeps each connection on the socket that
//! owns it; [`cpu`] pins those workers; [`udp`] (forked from quinn-udp) sends
//! and receives with GSO, GRO, and ECN. Both `moq-tokio` and `moq-uring` build
//! their worker groups on this crate, so the group-formation invariants live
//! here once, in the group itself rather than in each caller.

pub mod bind;
pub mod cpu;
pub mod shard;
// cargo fmt formats submodules with the crate root's config, so `just rs fix`
// formats this one separately with its own (quinn's).
#[rustfmt::skip]
pub mod udp;
