//! HTTP-server side: accept WHIP/WHEP offers from remote clients.
//!
//! Mounts axum routers that publish into [`moq_net::OriginProducer`] (WHIP
//! / `server publish`) and pull from [`moq_net::OriginConsumer`] (WHEP /
//! `server subscribe`). The HTTP listener itself is the caller's
//! responsibility; the `moq-cli` `rtc` subcommand mounts these under an
//! HTTP server.

pub mod whep;
pub mod whip;

mod mux;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, Uri};
use tokio::sync::{OnceCell, oneshot};

use crate::{Error, Result};
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

/// The negotiated session runner behind [`Response::run`]: holds the broadcast
/// announcement guard and mux registration for the session's lifetime, and
/// unregisters the session from the server registry on drop. Built by
/// [`whip::accept`] / [`whep::accept`] (descendant modules, so the private
/// fields are in scope there).
struct AcceptedSession {
	server: Server,
	resource_id: String,
	session: Option<crate::session::Session>,
	/// Announcement guard for the ingest (WHIP) path; unannounces on drop.
	/// `None` for egress (WHEP) sessions.
	publish: Option<moq_net::OriginPublish>,
	registration: Option<mux::Registration>,
	cancel: Option<oneshot::Receiver<()>>,
	role: &'static str,
}

impl AcceptedSession {
	async fn run(mut self) -> Result<()> {
		let session = self.session.take().expect("accepted session missing driver");
		let registration = self
			.registration
			.take()
			.expect("accepted session missing mux registration");
		let cancel = self.cancel.take().expect("accepted session missing cancel receiver");

		let result = {
			// Hold the announcement guard + mux registration for the session's
			// lifetime; both release (unannounce / unregister) on exit.
			let _publish = self.publish.take();
			let _registration = registration;
			tokio::select! {
				res = session.run() => {
					crate::session::log_session_end(self.role, &res);
					res
				}
				_ = cancel => {
					tracing::debug!(role = self.role, "webrtc session terminated by DELETE");
					Ok(())
				}
			}
		};
		normalize_session_result(result)
	}
}

impl Drop for AcceptedSession {
	fn drop(&mut self) {
		self.server.unregister_session(&self.resource_id);
	}
}

/// Fold an ordinary peer disconnect into `Ok` so [`Response::run`] only errors
/// on genuine failures.
fn normalize_session_result(result: Result<()>) -> Result<()> {
	match result {
		Ok(()) | Err(Error::SessionClosed) => Ok(()),
		Err(err) => Err(err),
	}
}

/// Build the `Location` header for a negotiated session by appending the
/// resource id to the request path, preserving whatever prefix the router is
/// mounted under.
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
#[derive(Clone, Debug)]
#[non_exhaustive]
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
	/// Live sessions keyed by resource id, each holding a cancel sender the
	/// session task selects on. Lets [`Server::terminate`] (and the bundled
	/// `DELETE` route) end a session by its `Location` id.
	sessions: Mutex<HashMap<String, oneshot::Sender<()>>>,
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
				sessions: Mutex::new(HashMap::new()),
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
pub(crate) async fn delete(State(server): State<Server>, Path(path): Path<String>) -> StatusCode {
	match crate::sdp::parse_resource_id(&path) {
		Ok(id) if server.terminate(&id.to_string()) => StatusCode::OK,
		Ok(_) => StatusCode::NOT_FOUND,
		Err(_) => StatusCode::BAD_REQUEST,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn server() -> Server {
		let publisher = moq_net::Origin::random().produce();
		let subscriber = moq_net::Origin::random().produce().consume();
		Server::new(Config::default(), publisher, subscriber)
	}

	#[test]
	fn terminate_unknown_session_is_false() {
		assert!(!server().terminate("00000000-0000-0000-0000-000000000000"));
	}

	#[test]
	fn terminate_registered_session_once() {
		let server = server();
		let id = "11111111-1111-1111-1111-111111111111";
		let _cancel = server.register_session(id.to_string());
		assert!(server.terminate(id), "first terminate finds the session");
		assert!(!server.terminate(id), "second terminate is a no-op");
	}

	#[test]
	fn unregister_drops_the_entry() {
		let server = server();
		let id = "22222222-2222-2222-2222-222222222222";
		let _cancel = server.register_session(id.to_string());
		server.unregister_session(id);
		assert!(!server.terminate(id), "unregistered session can't be terminated");
	}

	#[test]
	fn peer_close_is_a_successful_session_result() {
		assert!(normalize_session_result(Err(Error::SessionClosed)).is_ok());
	}

	#[test]
	fn session_location_preserves_mount_path() {
		let uri: Uri = "/whip/live/cam0?token=secret".parse().unwrap();
		let location = session_location(&uri, "session-id").expect("header value");
		assert_eq!(location, "/whip/live/cam0/session-id");
	}
}
