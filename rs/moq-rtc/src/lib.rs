//! WebRTC ↔ MoQ gateway.
//!
//! Accepts WHIP (RFC 9725) for ingestion and WHEP for egress, bridging
//! WebRTC media into a [`moq_net`] broadcast (and back). Built on
//! [`str0m`] for the sans-IO WebRTC stack and [`axum`] for the HTTP
//! signaling.
//!
//! ## Crate shape
//!
//! - [`whip`] mounts the `POST /<resource>` ingest endpoint.
//! - [`whep`] mounts the `POST /<resource>` egress endpoint.
//! - [`session`] runs the per-connection [`str0m::Rtc`] event loop and
//!   UDP socket.
//! - [`codec`] holds the per-codec bridges that convert depacketized
//!   media into [`moq_mux`] container frames.
//!
//! ## Public surface
//!
//! Library users build a [`Gateway`] with their [`moq_net::OriginProducer`]
//! and [`moq_net::OriginConsumer`] and mount the returned routers under
//! their own [`axum`] server. The `moq-rtc` binary is just a thin wrapper
//! that dials a relay and mounts both routers on its own listener.

pub mod codec;
mod error;
pub mod sdp;
pub mod session;
pub mod whep;
pub mod whip;

pub use error::*;

use std::sync::Arc;

use axum::Router;

/// Configuration for a [`Gateway`].
#[derive(Clone, Debug, Default)]
pub struct GatewayConfig {
	/// Public UDP socket addresses that should be advertised as ICE host
	/// candidates. Each is sent as a separate `candidate` line in the SDP
	/// answer so a remote peer can reach this gateway.
	///
	/// If empty, the session loop binds an ephemeral port and discovers the
	/// local address; that works for loopback testing but not behind NAT.
	pub ice_candidates: Vec<std::net::SocketAddr>,
}

/// Glue that owns the moq-net origin pair and hands axum routers to the caller.
///
/// `publisher` is where WHIP-ingested broadcasts get inserted; `subscriber`
/// is what WHEP requests fan out from. They're typically the two halves of
/// the same upstream [`moq_net::Session`].
#[derive(Clone)]
pub struct Gateway {
	inner: Arc<GatewayInner>,
}

struct GatewayInner {
	config: GatewayConfig,
	publisher: moq_net::OriginProducer,
	// Held for WHEP egress, which is gated until the per-codec re-packetizers land.
	#[allow(dead_code)]
	subscriber: moq_net::OriginConsumer,
}

impl Gateway {
	/// Build a gateway. `publisher` receives WHIP broadcasts; `subscriber`
	/// is the source for WHEP egress.
	pub fn new(config: GatewayConfig, publisher: moq_net::OriginProducer, subscriber: moq_net::OriginConsumer) -> Self {
		Self {
			inner: Arc::new(GatewayInner {
				config,
				publisher,
				subscriber,
			}),
		}
	}

	/// An axum router mounting the WHIP ingest endpoint at the root.
	///
	/// Path layout: `POST /<broadcast-path>` accepts an SDP offer and
	/// returns an SDP answer with a `Location` header pointing at the
	/// resource (used by WHIP clients for `DELETE` and `PATCH` calls).
	pub fn whip_router(&self) -> Router {
		whip::router(self.clone())
	}

	/// An axum router mounting the WHEP egress endpoint at the root.
	///
	/// Path layout: `POST /<broadcast-path>` accepts an SDP offer and
	/// returns an SDP answer; the matching broadcast must already be
	/// announced on the [`moq_net::OriginConsumer`].
	pub fn whep_router(&self) -> Router {
		whep::router(self.clone())
	}

	pub(crate) fn config(&self) -> &GatewayConfig {
		&self.inner.config
	}

	pub(crate) fn publisher(&self) -> &moq_net::OriginProducer {
		&self.inner.publisher
	}

	#[allow(dead_code)]
	pub(crate) fn subscriber(&self) -> &moq_net::OriginConsumer {
		&self.inner.subscriber
	}
}
