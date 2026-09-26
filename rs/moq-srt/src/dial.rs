//! SRT client (dial-out): connect to a remote SRT listener and bridge it to MoQ.
//!
//! The mirror of the crate's listener: where that binds a listener and accepts
//! callers, this *dials* a remote `srt://host:port` as an SRT caller and bridges
//! MPEG-TS in one of two directions, selected by the stream-id `m=` mode it sends:
//!
//! - **[`Client::publish`] (push / restream)**: call with `m=publish`, read a MoQ
//!   broadcast from an origin, re-mux it to MPEG-TS with [`moq_mux`], and send it
//!   to the remote listener. This restreams MoQ out to a remote SRT ingest.
//! - **[`Client::pull`] (ingest)**: call with `m=request`, receive the remote's
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

/// An SRT caller that can publish a MoQ broadcast or pull a remote stream.
///
/// Construct via [`Client::new`] and chain the `with_*` setters.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Client {
	/// The remote SRT listener to call.
	pub addr: SocketAddr,

	/// The resource to request, sent as the stream id's `r=` value. Must not contain
	/// `,` or `=`, which delimit the stream id.
	pub resource: String,

	/// SRT receive latency, negotiated at handshake time: the buffer that trades delay
	/// for loss recovery. It doubles as [`publish`]'s egress skip threshold.
	latency: Duration,

	/// How long relays keep a non-latest group of an ingested media track fetchable, or
	/// `None` for hang's own default.
	///
	/// A retention budget, not a delivery one: it never makes a subscriber play further
	/// behind live, it caps how far back a FETCH can still reach. The default suits a
	/// segmented egress (HLS/DASH) reading the broadcast downstream, which may only
	/// advertise segments that are still fetchable. Lower it when nothing reads history
	/// and the memory matters. [`pull`] only; [`publish`] reads a broadcast someone else
	/// declared.
	max_age: Option<Duration>,

	/// Connection allocator each ingested track claims its peak-hold bitrate on.
	/// [`pull`] only; [`publish`] reads a broadcast someone else declared.
	bandwidth: moq_net::bandwidth::Allocator,
}

impl Client {
	/// Dial `addr` for `resource`, with the default SRT latency (500ms) and the
	/// publisher's own media retention.
	pub fn new(addr: SocketAddr, resource: impl Into<String>) -> Self {
		Self {
			addr,
			resource: resource.into(),
			latency: DEFAULT_LATENCY,
			max_age: None,
			bandwidth: moq_net::bandwidth::Allocator::unlimited(),
		}
	}

	/// Override the SRT receive latency negotiated at handshake time.
	pub fn with_latency(mut self, latency: Duration) -> Self {
		self.latency = latency;
		self
	}

	/// Set how long non-latest groups created by [`pull`](Self::pull) remain fetchable.
	pub fn with_max_age(mut self, max_age: Option<Duration>) -> Self {
		self.max_age = max_age;
		self
	}

	/// Claim each track ingested by [`pull`](Self::pull) on `bandwidth`.
	pub fn with_bandwidth(mut self, bandwidth: moq_net::bandwidth::Allocator) -> Self {
		self.bandwidth = bandwidth;
		self
	}

	/// Push a MoQ broadcast out to the remote as MPEG-TS until the broadcast ends.
	pub async fn publish(&self, origin: &origin::Consumer, path: impl moq_net::AsPath) -> Result<()> {
		let path = path.as_path();
		let socket = self.call(Mode::Publish).await?;
		serve_subscribe(origin, path.as_str(), socket, self.latency).await
	}

	/// Pull a remote MPEG-TS stream into `origin` at `path` until the remote ends.
	pub async fn pull(&self, origin: &origin::Producer, path: impl moq_net::AsPath) -> Result<()> {
		let path = path.as_path();
		let socket = self.call(Mode::Request).await?;
		let catalog = moq_mux::catalog::Config::default()
			.with_max_age(self.max_age)
			.with_bandwidth(self.bandwidth.clone());
		serve_publish(origin, path.as_str(), socket, catalog).await
	}

	async fn call(&self, mode: Mode) -> Result<SrtSocket> {
		if self.resource.contains([',', '=']) {
			return Err(crate::Error::InvalidResource(self.resource.clone()));
		}
		let stream_id = format!("#!::r={},m={}", self.resource, mode.as_str());
		let socket = SrtSocket::builder()
			.latency(self.latency)
			.set(configure_buffers)
			.call(self.addr, Some(&stream_id))
			.await?;
		tracing::info!(addr = %self.addr, resource = %self.resource, mode = mode.as_str(), "SRT caller connected");
		Ok(socket)
	}
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
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(moq_net::time::run(driver));
		} else {
			// A sync test: nothing polls the driver, and dropping it would tear
			// the origin down, so leak it and rely on the synchronous half.
			std::mem::forget(driver);
		}
		producer
	}

	use std::io;
	use std::net::SocketAddr;
	use std::time::Duration;

	use srt_protocol::protocol::pending_connection::ConnectionReject;
	use srt_tokio::access::RejectReason;

	use super::*;
	use crate::server::{Reject, Request, Server};

	/// An SRT server on an ephemeral loopback port.
	async fn loopback() -> (Server, SocketAddr) {
		let server = Server::bind("127.0.0.1:0".parse().unwrap(), None).await.unwrap();
		let addr = server.local_addr();
		(server, addr)
	}

	/// Loopback: dial the crate's own server with `m=publish`. The server classifies it
	/// as a publish, accepts it (completing the SRT handshake), and the caller connects.
	/// Proves the new caller path: handshake + the `#!::r=..,m=publish` stream id routing
	/// to a server [`Request::Publish`]. (A full MoQ->TS->MoQ media round-trip is left to
	/// integration coverage; the TS bridge itself is shared with the tested server path.)
	#[tokio::test]
	async fn publish_caller_connects_and_routes() {
		let (mut server, addr) = loopback().await;

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
		let caller = tokio::spawn(async move { Client::new(addr, "cam0").call(Mode::Publish).await });

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
		let (mut server, addr) = loopback().await;

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

		let caller = tokio::spawn(async move { Client::new(addr, "cam0").call(Mode::Request).await });

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

	/// Reject a caller server-side and hand back the error it saw.
	///
	/// The server is driven inline rather than on its own task: [`Publish::reject`]
	/// only hands the verdict to the listener, so dropping the [`Server`] before the
	/// caller's handshake completes stops the listener that still owes it the
	/// rejection packet, and the caller times out instead.
	async fn rejected(mode: Mode, reason: Reject, code: i32) {
		let (mut server, addr) = loopback().await;
		let client = Client::new(addr, "cam0");

		let (_, err) = tokio::join!(
			async {
				match (mode, server.accept().await.expect("a request")) {
					(Mode::Publish, Request::Publish(request)) => request.reject(reason).await.unwrap(),
					(Mode::Request, Request::Subscribe(request)) => request.reject(reason).await.unwrap(),
					_ => panic!("request routed in the wrong direction"),
				}
			},
			client.call(mode),
		);

		let err = match err {
			Ok(_) => panic!("rejected SRT caller connected"),
			Err(err) => err,
		};
		let crate::Error::Io(err) = err else {
			panic!("SRT rejection was not an I/O error: {err}");
		};
		assert_eq!(err.kind(), io::ErrorKind::ConnectionRefused);
		assert_eq!(
			err.get_ref().and_then(|err| err.downcast_ref::<ConnectionReject>()),
			Some(&ConnectionReject::Rejected(RejectReason::CoreUnrecognized(code)))
		);
	}

	#[tokio::test]
	async fn publish_rejection_carries_unauthorized_code() {
		rejected(Mode::Publish, Reject::Unauthorized, 1401).await;
	}

	#[tokio::test]
	async fn subscribe_rejection_carries_unavailable_code() {
		rejected(Mode::Request, Reject::Unavailable, 1503).await;
	}
}
