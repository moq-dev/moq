//! A thread-per-core io_uring worker for the native MoQ stack.
//!
//! One [`Worker`] per pinned thread. It owns a `SINGLE_ISSUER | DEFER_TASKRUN |
//! COOP_TASKRUN` ring, a userspace timer heap, a local task set, and the UDP
//! sockets bound through it. The caller owns the thread loop: drive everything
//! with [`Worker::block_on`], spawn extra `!Send` tasks through [`Handle`], and
//! wake the worker from other threads through any [`std::task::Waker`] it hands
//! out (a futex word, no ring or syscall needed while the worker is awake).
//!
//! UDP is the point: [`udp::Socket`] receives through one multishot `recvmsg`
//! with a registered provided-buffer ring (one whole buffer per completion,
//! `UDP_GRO` coalesced) and sends with an explicit `UDP_SEGMENT` control
//! message per `sendmsg` from a fixed pool of staging buffers.
//!
//! [`quic`] stacks a sans-IO QUIC stack on that path: a [`quic::Endpoint`]
//! serves many connections on one socket (demuxed by connection id, dials
//! included), each a [`quic::Connection`] implementing the transport traits,
//! so `moq_net::Client::connect_lite` and `Server::accept_lite` run real
//! moq-lite sessions on the worker ([`Handle`] is their
//! [`moq_net::Runtime`]). The stack underneath is the `quiche` (default) or
//! `quinn` feature; the module is the same either way, and a build with
//! neither leaves it out.
//!
//! Requires Linux 6.12; [`Worker::new`] refuses older kernels with a legible
//! error instead of degrading. The crate compiles to nothing off Linux.
// Off Linux the crate compiles to nothing, so these doc links have no target.
#![cfg_attr(not(target_os = "linux"), allow(rustdoc::broken_intra_doc_links))]
#![cfg(target_os = "linux")]

mod error;
mod park;
#[cfg(any(feature = "quiche", feature = "quinn"))]
pub mod quic;
mod shared;
mod timer;
pub mod udp;
mod worker;

pub use error::Error;
pub use timer::Timer;
pub use worker::{Config, Handle, Worker};

/// The `url` crate, re-exported because
/// [`quic::web::Request::url`](crate::quic::web::Request::url) hands one back.
/// Naming that type otherwise means depending on a matching `url` version
/// directly, so a major bump here is a breaking change for this crate.
pub use url;
