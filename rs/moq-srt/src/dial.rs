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

use moq_net::origin;
use srt_tokio::SrtSocket;

use crate::Result;
use crate::server::{DEFAULT_LATENCY, configure_buffers, serve_publish, serve_subscribe};

/// Where to dial and how, shared by [`publish`] and [`pull`].
///
/// Construct via [`Config::new`] and set the fields you need, so new options stay
/// additive.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
	/// The remote SRT listener to call.
	pub addr: SocketAddr,

	/// The resource to request, sent as the stream id's `r=` value. Must not contain
	/// `,` or `=`, which delimit the stream id.
	pub resource: String,

	/// SRT receive latency, negotiated at handshake time: the buffer that trades delay
	/// for loss recovery. It doubles as [`publish`]'s egress skip threshold.
	pub latency: Duration,

	/// How long relays keep a non-latest group of an ingested media track fetchable, or
	/// `None` for hang's own default.
	///
	/// A retention budget, not a delivery one: it never makes a subscriber play further
	/// behind live, it caps how far back a FETCH can still reach. The default suits a
	/// segmented egress (HLS/DASH) reading the broadcast downstream, which may only
	/// advertise segments that are still fetchable. Lower it when nothing reads history
	/// and the memory matters. [`pull`] only; [`publish`] reads a broadcast someone else
	/// declared.
	pub max_age: Option<Duration>,
}

impl Config {
	/// Dial `addr` for `resource`, with the default SRT latency (500ms) and the
	/// publisher's own media retention.
	pub fn new(addr: SocketAddr, resource: impl Into<String>) -> Self {
		Self {
			addr,
			resource: resource.into(),
			latency: DEFAULT_LATENCY,
			max_age: None,
		}
	}
}

/// Push a MoQ broadcast out to the remote: connect as an SRT caller requesting the
/// remote receive on [`Config::resource`] (`m=publish`), re-mux `path` from `origin`
/// to MPEG-TS, and send it until the broadcast ends.
///
/// This future resolves when the broadcast ends, so callers usually run it on its own
/// task.
pub async fn publish(config: &Config, origin: &origin::Consumer, path: impl moq_net::AsPath) -> Result<()> {
	let path = path.as_path();
	let socket = call(config, Mode::Publish).await?;
	serve_subscribe(origin, path.as_str(), socket, config.latency).await
}

/// Pull a remote stream into `origin`: connect as an SRT caller requesting the remote
/// send on [`Config::resource`] (`m=request`), demux its MPEG-TS, and publish the
/// result at `path` until the remote ends.
///
/// This future resolves when the remote stream ends, so callers usually run it on its
/// own task.
pub async fn pull(config: &Config, origin: &origin::Producer, path: impl moq_net::AsPath) -> Result<()> {
	let path = path.as_path();
	let socket = call(config, Mode::Request).await?;
	serve_publish(origin, path.as_str(), socket, config.max_age).await
}

/// Dial as an SRT caller, sending the standard `#!::r=<resource>,m=<mode>` stream id
/// and returning the connected socket.
///
/// `mode` is the *remote's* role, the inverse of the local direction (the remote
/// receives on `m=publish`, sends on `m=request`).
async fn call(config: &Config, mode: Mode) -> Result<SrtSocket> {
	let Config { addr, resource, .. } = config;
	// `,` and `=` delimit the `#!::r=<resource>,m=<mode>` stream id, so a resource
	// carrying either would corrupt it and misroute at the listener. Reject rather
	// than silently produce a broken id (MoQ paths never contain these).
	if resource.contains([',', '=']) {
		return Err(anyhow::anyhow!("srt resource must not contain ',' or '=': {resource:?}").into());
	}
	let stream_id = format!("#!::r={resource},m={}", mode.as_str());
	let socket = SrtSocket::builder()
		.latency(config.latency)
		.set(configure_buffers)
		.call(*addr, Some(&stream_id))
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
	/// Build an origin producer, spawning its driver on the ambient runtime.
	fn produce_origin() -> moq_net::origin::Producer {
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::Origin::random().into());
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(driver);
		} else {
			// A sync test: nothing polls the driver, and dropping it would tear
			// the origin down, so leak it and rely on the synchronous half.
			std::mem::forget(driver);
		}
		producer
	}

	use std::net::SocketAddr;
	use std::time::Duration;

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
		let origin = produce_origin();
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
		let caller = tokio::spawn(async move { call(&Config::new(addr, "cam0"), Mode::Publish).await });

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
		let origin = produce_origin();
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

		let caller = tokio::spawn(async move { call(&Config::new(addr, "cam0"), Mode::Request).await });

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
