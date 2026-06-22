//! HTTP-server side: accept WHIP/WHEP offers from remote clients.
//!
//! Mounts axum routers that publish into [`moq_net::OriginProducer`] (WHIP
//! / `server publish`) and pull from [`moq_net::OriginConsumer`] (WHEP /
//! `server subscribe`). The HTTP listener itself is the caller's
//! responsibility; the binary in `bin/moq-rtc.rs` mounts these under
//! axum_server.

pub mod whep;
pub mod whip;

mod mux;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use tokio::sync::OnceCell;

use crate::Result;
use mux::Mux;

/// The result of a WHIP/WHEP [`whip::accept`] / [`whep::accept`]: the SDP answer
/// to return to the client, plus an opaque resource id for the `Location` header
/// (the RFC 9725 session resource URL).
#[derive(Clone, Debug)]
pub struct Response {
	/// Opaque id identifying the negotiated session, for the `Location` header.
	pub resource_id: String,
	/// The SDP answer body (`Content-Type: application/sdp`).
	pub answer: String,
}

/// Configuration shared by both `server publish` and `server subscribe`.
#[derive(Clone, Debug)]
pub struct Config {
	/// Public UDP socket addresses that should be advertised as ICE host
	/// candidates. Each is sent as a separate `candidate` line in the SDP
	/// answer so a remote peer can reach us.
	///
	/// If empty, the mux advertises whatever address the OS picked for the
	/// shared socket. That works for loopback testing but not behind NAT.
	pub ice_candidates: Vec<SocketAddr>,

	/// Address the shared WebRTC media socket binds to. Every WHIP/WHEP session
	/// shares this one UDP port (demuxed by ICE ufrag), so a deployment opens
	/// exactly one media port in its firewall. `0.0.0.0:0` (the default) lets
	/// the OS pick a port, which is fine for dev/loopback; production pins it.
	pub udp_bind: SocketAddr,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			ice_candidates: Vec::new(),
			udp_bind: SocketAddr::from(([0, 0, 0, 0], 0)),
		}
	}
}

/// Glue that owns the moq-net origin pair and hands axum routers to the caller.
///
/// `publisher` is where `server publish` (WHIP) writes ingested broadcasts;
/// `subscriber` is what `server subscribe` (WHEP) reads from. They're
/// typically the two halves of the same upstream [`moq_net::Session`].
#[derive(Clone)]
pub struct Server {
	inner: Arc<Inner>,
}

struct Inner {
	config: Config,
	publisher: moq_net::OriginProducer,
	/// Source for `server subscribe` (WHEP) egress.
	subscriber: moq_net::OriginConsumer,
	/// The shared media socket + demux, bound lazily on the first accept so
	/// `Server::new` can stay synchronous (and an idle server binds no port).
	mux: OnceCell<Mux>,
}

impl Server {
	/// Build a server. `publisher` receives WHIP broadcasts; `subscriber`
	/// is the source for WHEP egress.
	pub fn new(config: Config, publisher: moq_net::OriginProducer, subscriber: moq_net::OriginConsumer) -> Self {
		Self {
			inner: Arc::new(Inner {
				config,
				publisher,
				subscriber,
				mux: OnceCell::new(),
			}),
		}
	}

	/// The shared media mux, bound (and its demux task spawned) on first use.
	pub(crate) async fn mux(&self) -> Result<&Mux> {
		self.inner
			.mux
			.get_or_try_init(|| Mux::bind(self.inner.config.udp_bind, &self.inner.config.ice_candidates))
			.await
	}

	/// Router for `server publish` (WHIP). Mount under whichever HTTP path
	/// the deployment prefers (`/whip`, `/`, ...).
	///
	/// The router derives the broadcast name from the request path and performs
	/// no authentication. To own the route and authorize requests yourself
	/// (resolving the broadcast name from a verified token), skip the router and
	/// call [`whip::accept`] directly from your own handler.
	pub fn publish_router(&self) -> Router {
		whip::router(self.clone())
	}

	/// Router for `server subscribe` (WHEP). Mount under whichever HTTP path
	/// the deployment prefers (`/whep`, `/`, ...).
	///
	/// The router derives the broadcast name from the request path and performs
	/// no authentication. To own the route and authorize requests yourself
	/// (resolving the broadcast name from a verified token), skip the router and
	/// call [`whep::accept`] directly from your own handler.
	pub fn subscribe_router(&self) -> Router {
		whep::router(self.clone())
	}

	pub(crate) fn publisher(&self) -> &moq_net::OriginProducer {
		&self.inner.publisher
	}

	pub(crate) fn subscriber(&self) -> &moq_net::OriginConsumer {
		&self.inner.subscriber
	}
}
