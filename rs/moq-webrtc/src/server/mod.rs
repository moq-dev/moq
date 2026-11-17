//! HTTP-server side: accept WHIP/WHEP offers from remote clients.
//!
//! Accepts offers that publish into [`moq_net::origin::Producer`] (WHIP
//! / `server publish`) and pull from [`moq_net::origin::Consumer`] (WHEP /
//! `server subscribe`). The HTTP listener itself is the caller's
//! responsibility; the `moq-cli` `rtc` subcommand mounts the bundled axum
//! routers (the `server` feature) under an HTTP server.

pub mod whep;
pub mod whip;

mod mux;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[cfg(feature = "server")]
use axum::Router;
#[cfg(feature = "server")]
use axum::http::{HeaderValue, StatusCode, Uri};
use tokio::sync::oneshot;

use crate::Result;
use mux::Mux;

/// The result of a WHIP/WHEP [`whip::accept`] / [`whep::accept`]: the SDP answer
/// to return to the client, plus an opaque resource id for the `Location` header
/// (the RFC 9725 session resource URL).
pub struct Response {
	/// Opaque id identifying the negotiated session, for the `Location` header.
	pub resource_id: String,
	/// The SDP answer body (`Content-Type: application/sdp`).
	pub answer: String,
	session: AcceptedSession,
}

impl Response {
	/// Run the negotiated media session until the peer disconnects, DELETE terminates it, or it errors.
	pub async fn run(self) -> Result<()> {
		self.session.run().await
	}
}

impl std::fmt::Debug for Response {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Response")
			.field("resource_id", &self.resource_id)
			.field("answer", &self.answer)
			.finish_non_exhaustive()
	}
}

/// The negotiated session runner behind [`Response::run`]: holds the mux
/// registration for the session's lifetime, and unregisters the session from
/// the server registry on drop. Built by [`whip::accept`] / [`whep::accept`]
/// (descendant modules, so the private fields are in scope there).
struct AcceptedSession {
	server: Server,
	resource_id: String,
	session: Option<crate::session::Session>,
	registration: Option<mux::Registration>,
	cancel: Option<oneshot::Receiver<()>>,
	/// How long an ingest the peer ended with a `DELETE` stays up.
	linger: Duration,
	role: &'static str,
}

impl AcceptedSession {
	async fn run(mut self) -> Result<()> {
		let mut session = self.session.take().expect("accepted session missing driver");
		let registration = self
			.registration
			.take()
			.expect("accepted session missing mux registration");
		let cancel = self.cancel.take().expect("accepted session missing cancel receiver");

		let ended = {
			// Hold the mux registration while media flows; it unregisters on exit.
			let _registration = registration;
			tokio::select! {
				// A client may close its peer connection right after its DELETE; the DELETE
				// still names the end as clean.
				biased;
				_ = cancel => None,
				res = session.run() => Some(res),
			}
		};
		if let Some(res) = ended {
			return crate::session::end(self.role, res);
		}

		// A DELETE is the peer's clean end after its final media. An ingest serves the
		// finished broadcast for the linger, so a subscriber that arrives after the end still
		// gets the tail; dropping this future ends that early. A peer that vanishes instead
		// closes the broadcast at once.
		tracing::debug!(role = self.role, "webrtc session terminated by DELETE");
		let linger = self.linger;
		if let Some(finished) = session.finish(linger)? {
			tracing::info!(role = self.role, ?linger, "serving the finished broadcast");
			finished.wait().await;
		}
		Ok(())
	}
}

impl Drop for AcceptedSession {
	fn drop(&mut self) {
		self.server.unregister_session(&self.resource_id);
	}
}

/// Build the `Location` header for a negotiated session by appending the
/// resource id to the request path, preserving whatever prefix the router is
/// mounted under.
#[cfg(feature = "server")]
pub(crate) fn session_location(uri: &Uri, resource_id: &str) -> Option<HeaderValue> {
	let base = uri.path().trim_end_matches('/');
	let path = if base.is_empty() {
		format!("/{resource_id}")
	} else {
		format!("{base}/{resource_id}")
	};
	HeaderValue::from_str(&path).ok()
}

/// Configuration shared by both `server publish` and `server subscribe`.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Public UDP socket addresses advertised as ICE host candidates, each sent
	/// as given in its own `candidate` line of the SDP answer. A port of 0 takes
	/// the bound port, so a public IP can sit in front of an OS-picked port.
	///
	/// If empty, the server advertises the bound address, substituting loopback
	/// when the socket is bound to an unspecified address. That works for
	/// loopback testing but not behind NAT.
	pub ice_candidates: Vec<SocketAddr>,
}

/// Shared WebRTC media state: the UDP mux and live sessions behind [`whip::accept`] and [`whep::accept`].
#[derive(Clone)]
pub struct Server {
	inner: Arc<Inner>,
}

struct Inner {
	/// The shared media socket and its demux. Every WHIP/WHEP session shares
	/// this one UDP port, demuxed by ICE ufrag.
	mux: Mux,
	/// Live sessions keyed by resource id, each holding a cancel sender the
	/// session task selects on. Lets [`Server::terminate`] (and the bundled
	/// `DELETE` route) end a session by its `Location` id.
	sessions: Mutex<HashMap<String, oneshot::Sender<()>>>,
}

impl Server {
	/// Bind the shared media socket at `addr` and serve WHIP and WHEP sessions on it.
	///
	/// Every session shares this one UDP port, so a deployment opens exactly one
	/// media port in its firewall; port 0 lets the OS pick one. Fails if the
	/// socket cannot be bound, or an advertised candidate is invalid. Dropping
	/// the last clone, and every session it accepted, releases the port.
	pub async fn bind(addr: SocketAddr, config: Config) -> Result<Self> {
		let mux = Mux::bind(addr, &config.ice_candidates).await?;
		Ok(Self {
			inner: Arc::new(Inner {
				mux,
				sessions: Mutex::new(HashMap::new()),
			}),
		})
	}

	/// The shared media mux.
	pub(crate) fn mux(&self) -> &Mux {
		&self.inner.mux
	}

	/// The address the shared media socket is bound to.
	pub fn local_addr(&self) -> SocketAddr {
		self.inner.mux.local_addr()
	}

	/// Router for `server publish` (WHIP), publishing each session as `ingest` says. Mount
	/// under whichever HTTP path the deployment prefers (`/whip`, `/`, ...).
	///
	/// The router derives the broadcast name from the request path and performs
	/// no authentication. To own the route and authorize requests yourself
	/// (resolving the broadcast name from a verified token), skip the router and
	/// call [`whip::accept`] directly from your own handler. Only with the `server` feature.
	#[cfg(feature = "server")]
	pub fn publish_router(&self, publisher: moq_net::origin::Producer, ingest: moq_mux::Ingest) -> Router {
		whip::router(self.clone(), publisher, ingest)
	}

	/// Router for `server subscribe` (WHEP). Mount under whichever HTTP path
	/// the deployment prefers (`/whep`, `/`, ...).
	///
	/// The router derives the broadcast name from the request path and performs
	/// no authentication. To own the route and authorize requests yourself
	/// (resolving the broadcast name from a verified token), skip the router and
	/// call [`whep::accept`] directly from your own handler. Only with the `server` feature.
	#[cfg(feature = "server")]
	pub fn subscribe_router(&self, subscriber: moq_net::origin::Consumer) -> Router {
		whep::router(self.clone(), subscriber)
	}

	/// Register a session under its resource id, returning the cancel receiver.
	/// Called by [`whip::accept`] / [`whep::accept`] before returning the
	/// negotiated session runner.
	pub(crate) fn register_session(&self, resource_id: String) -> oneshot::Receiver<()> {
		let (tx, rx) = oneshot::channel();
		self.inner.sessions.lock().unwrap().insert(resource_id, tx);
		rx
	}

	/// Drop a session's registry entry once it has ended on its own.
	pub(crate) fn unregister_session(&self, resource_id: &str) {
		self.inner.sessions.lock().unwrap().remove(resource_id);
	}

	/// Terminate a negotiated session by its resource id (the `Location` path
	/// component from the WHIP/WHEP response). Returns `true` if a live session
	/// was found and signalled to stop; the session task then releases its
	/// broadcast announcement and mux registration. Embedders that own their own
	/// HTTP routing call this to honor a WHIP/WHEP `DELETE`; the bundled routers
	/// already wire it to the `DELETE` method.
	pub fn terminate(&self, resource_id: &str) -> bool {
		if let Some(cancel) = self.inner.sessions.lock().unwrap().remove(resource_id) {
			let _ = cancel.send(());
			true
		} else {
			false
		}
	}
}

/// Shared `DELETE` handler for both bundled routers: parse the resource id from
/// the trailing path segment and terminate the matching session.
#[cfg(feature = "server")]
pub(crate) fn delete(server: &Server, path: &str) -> StatusCode {
	match crate::sdp::parse_resource_id(path) {
		Ok(id) if server.terminate(&id.to_string()) => StatusCode::OK,
		Ok(_) => StatusCode::NOT_FOUND,
		Err(_) => StatusCode::BAD_REQUEST,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	const LOOPBACK: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 0);

	async fn server() -> Server {
		Server::bind(LOOPBACK, Config::default()).await.unwrap()
	}

	/// A port something else holds fails the constructor, before anything reports
	/// the server ready, rather than the first WHIP or WHEP request.
	#[tokio::test]
	async fn bind_fails_on_a_taken_port() {
		let taken = std::net::UdpSocket::bind(LOOPBACK).unwrap();
		let err = Server::bind(taken.local_addr().unwrap(), Config::default())
			.await
			.err()
			.expect("bound a taken port");
		assert!(matches!(err, crate::Error::Io(_)), "{err}");
	}

	/// The last clone of a server that accepted nothing releases its port.
	#[tokio::test]
	async fn dropping_the_server_releases_its_port() {
		let server = server().await;
		let addr = server.local_addr();
		let clone = server.clone();
		drop(server);
		std::net::UdpSocket::bind(addr).expect_err("a live clone keeps the port");

		drop(clone);
		// The abort only schedules the demux task to be dropped; yielding lets this
		// current-thread runtime run it, which drops its socket handle.
		tokio::task::yield_now().await;
		std::net::UdpSocket::bind(addr).expect("the dropped server kept its port");
	}

	#[tokio::test]
	async fn terminate_unknown_session_is_false() {
		assert!(!server().await.terminate("00000000-0000-0000-0000-000000000000"));
	}

	#[tokio::test]
	async fn terminate_registered_session_once() {
		let server = server().await;
		let id = "11111111-1111-1111-1111-111111111111";
		let _cancel = server.register_session(id.to_string());
		assert!(server.terminate(id), "first terminate finds the session");
		assert!(!server.terminate(id), "second terminate is a no-op");
	}

	#[tokio::test]
	async fn unregister_drops_the_entry() {
		let server = server().await;
		let id = "22222222-2222-2222-2222-222222222222";
		let _cancel = server.register_session(id.to_string());
		server.unregister_session(id);
		assert!(!server.terminate(id), "unregistered session can't be terminated");
	}

	#[cfg(feature = "server")]
	#[test]
	fn session_location_preserves_mount_path() {
		let uri: Uri = "/whip/live/cam0?token=secret".parse().unwrap();
		let location = session_location(&uri, "session-id").expect("header value");
		assert_eq!(location, "/whip/live/cam0/session-id");
	}
}
