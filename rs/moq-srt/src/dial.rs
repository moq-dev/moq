//! SRT client (dial-out): connect to a remote SRT listener and bridge it to MoQ.
//!
//! The mirror of the crate's listener: where that binds a listener and accepts
//! callers, this *dials* a remote `srt://host:port` as an SRT caller and bridges
//! MPEG-TS in one of two directions, selected by the stream-id `m=` mode it sends:
//!
//! - **[`publish`] (push / restream)**: call with `m=publish`, read a MoQ
//!   broadcast from an origin, re-mux it to MPEG-TS with [`moq_mux`], and send it
//!   to the remote listener. This restreams MoQ out to a remote SRT ingest.
//! - **[`pull`] (ingest)**: call with `m=request`, receive the remote's
//!   MPEG-TS, demux it with [`moq_mux`], and publish the result into an origin as
//!   an ordinary MoQ broadcast. This ingests a remote SRT source.
//!
//! It reuses the same MPEG-TS <-> moq bridge and the server's
//! per-frame pacing; only the SRT caller transport is new. The `m=` mode we *send*
//! is the remote's view (it publishes to us on `m=request`, receives from us on
//! `m=publish`), the inverse of the local direction: a local pull asks the remote
//! to send (`m=request`), a local push tells the remote to receive (`m=publish`).

use std::net::SocketAddr;
use std::time::Duration;

use moq_net::{OriginConsumer, OriginProducer};
use srt_tokio::SrtSocket;

use crate::Result;
use crate::server::{DEFAULT_LATENCY, serve_publish, serve_subscribe};

/// Dial `addr` and push a MoQ broadcast out to the remote: connect as an SRT caller
/// requesting the remote receive on `resource` (`m=publish`), re-mux `path` from
/// `origin` to MPEG-TS, and send it until the broadcast ends.
///
/// `latency` is the SRT receive latency negotiated at handshake time; pass `None`
/// for the default (200ms). This future resolves when the broadcast ends, so
/// callers usually run it on its own task.
pub async fn publish(
	addr: SocketAddr,
	resource: &str,
	latency: impl Into<Option<Duration>>,
	origin: &OriginConsumer,
	path: &str,
) -> Result<()> {
	let latency = latency.into().unwrap_or(DEFAULT_LATENCY);
	let socket = call(addr, resource, Mode::Publish, latency).await?;
	serve_subscribe(origin, path, socket, latency).await
}

/// Dial `addr` and pull a remote stream into `origin`: connect as an SRT caller
/// requesting the remote send on `resource` (`m=request`), demux its MPEG-TS, and
/// publish the result at `path` until the remote ends.
///
/// `latency` is the SRT receive latency negotiated at handshake time; pass `None`
/// for the default (200ms). This future resolves when the remote stream ends, so
/// callers usually run it on its own task.
pub async fn pull(
	addr: SocketAddr,
	resource: &str,
	latency: impl Into<Option<Duration>>,
	origin: &OriginProducer,
	path: &str,
) -> Result<()> {
	let socket = call(addr, resource, Mode::Request, latency).await?;
	serve_publish(origin, path, socket).await
}

/// Dial `addr` as an SRT caller for `resource`, sending the standard
/// `#!::r=<resource>,m=<mode>` stream id and returning the connected socket.
///
/// `mode` is the *remote's* role, the inverse of the local direction (the remote
/// receives on `m=publish`, sends on `m=request`).
async fn call(addr: SocketAddr, resource: &str, mode: Mode, latency: impl Into<Option<Duration>>) -> Result<SrtSocket> {
	// `,` and `=` delimit the `#!::r=<resource>,m=<mode>` stream id, so a resource
	// carrying either would corrupt it and misroute at the listener. Reject rather
	// than silently produce a broken id (MoQ paths never contain these).
	if resource.contains([',', '=']) {
		return Err(anyhow::anyhow!("srt resource must not contain ',' or '=': {resource:?}").into());
	}
	let latency = latency.into().unwrap_or(DEFAULT_LATENCY);
	let stream_id = format!("#!::r={resource},m={}", mode.as_str());
	let socket = SrtSocket::builder()
		.latency(latency)
		.call(addr, Some(&stream_id))
		.await?;
	tracing::info!(%addr, %resource, mode = mode.as_str(), "SRT caller connected");
	Ok(socket)
}

/// The SRT stream-id `m=` mode sent to the remote, i.e. the remote's role.
#[derive(Clone, Copy)]
enum Mode {
	/// `m=publish`: the remote receives media from us (a local push).
	Publish,
	/// `m=request`: the remote sends media to us (a local pull).
	Request,
}

impl Mode {
	fn as_str(self) -> &'static str {
		match self {
			Mode::Publish => "publish",
			Mode::Request => "request",
		}
	}
}

#[cfg(test)]
mod tests {
	use std::net::SocketAddr;
	use std::time::Duration;

	use moq_net::Origin;

	use super::*;
	use crate::server::{Request, Server};

	/// Grab a free UDP port by binding `:0` and releasing it. Racy in principle, but
	/// the window before the SRT server rebinds it is tiny; good enough for a test.
	async fn free_udp_addr() -> SocketAddr {
		let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
		sock.local_addr().unwrap()
	}

	/// Loopback: dial the crate's own server with `m=publish`. The server classifies it
	/// as a publish, accepts it (completing the SRT handshake), and the caller connects.
	/// Proves the new caller path: handshake + the `#!::r=..,m=publish` stream id routing
	/// to a server [`Request::Publish`]. (A full MoQ->TS->MoQ media round-trip is left to
	/// integration coverage; the TS bridge itself is shared with the tested server path.)
	#[tokio::test]
	async fn publish_caller_connects_and_routes() {
		let addr = free_udp_addr().await;
		let mut server = Server::bind(addr, None).await.unwrap();

		// Server accepts the publish so the caller's handshake completes; it ingests into
		// a throwaway origin and returns the routed direction + resource.
		let origin = Origin::random().produce();
		let server_task = tokio::spawn(async move {
			let request = server.accept().await.expect("a request");
			let resource = request.resource().to_string();
			let is_publish = matches!(request, Request::Publish(_));
			if let Request::Publish(publish) = request {
				// Runs until the caller disconnects; the test aborts it.
				publish.accept(&origin, "ingested/cam0").await.ok();
			}
			(resource, is_publish)
		});

		// Caller: dial with m=publish, then drop (we only assert connect + routing).
		let caller = tokio::spawn(async move { call(addr, "cam0", Mode::Publish, None).await });

		let socket = tokio::time::timeout(Duration::from_secs(10), caller)
			.await
			.expect("caller timed out")
			.expect("caller task")
			.expect("SRT caller should connect");
		drop(socket);

		let (resource, is_publish) = tokio::time::timeout(Duration::from_secs(10), server_task)
			.await
			.expect("server timed out")
			.expect("server task");
		assert_eq!(resource, "cam0");
		assert!(is_publish, "m=publish should route to a server Publish request");
	}

	/// Loopback: dial with `m=request`; the server classifies it as a subscribe and
	/// accepts it, so the caller connects. Proves the `#!::r=..,m=request` stream id
	/// routes to a server [`Request::Subscribe`].
	#[tokio::test]
	async fn request_caller_connects_and_routes() {
		let addr = free_udp_addr().await;
		let mut server = Server::bind(addr, None).await.unwrap();

		// Empty origin: the subscribe accept parks waiting for the broadcast, which is
		// fine -- the caller still connects, and the test aborts the wait.
		let origin = Origin::random().produce();
		let consumer = origin.consume();
		let server_task = tokio::spawn(async move {
			let request = server.accept().await.expect("a request");
			let resource = request.resource().to_string();
			let is_subscribe = matches!(request, Request::Subscribe(_));
			if let Request::Subscribe(subscribe) = request {
				subscribe.accept(&consumer, "live/cam0").await.ok();
			}
			(resource, is_subscribe)
		});

		let caller = tokio::spawn(async move { call(addr, "cam0", Mode::Request, None).await });

		let socket = tokio::time::timeout(Duration::from_secs(10), caller)
			.await
			.expect("caller timed out")
			.expect("caller task")
			.expect("SRT caller should connect");
		drop(socket);

		let (resource, is_subscribe) = tokio::time::timeout(Duration::from_secs(10), server_task)
			.await
			.expect("server timed out")
			.expect("server task");
		assert_eq!(resource, "cam0");
		assert!(is_subscribe, "m=request should route to a server Subscribe request");
	}
}
