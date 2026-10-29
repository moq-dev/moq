//! `server subscribe`: WHEP server.
//!
//! `POST /<broadcast-path>` accepts a WHEP SDP offer and returns an SDP
//! answer sourced from the matching MoQ broadcast on the subscribe origin.

#[cfg(feature = "server")]
use axum::{
	Router,
	body::Bytes,
	extract::{OriginalUri, Path, State},
	http::{HeaderMap, HeaderValue, StatusCode, header},
	response::{IntoResponse, Response as HttpResponse},
	routing::post,
};
use str0m::Candidate;

use std::time::Duration;

use crate::{Error, Result, egress::EgressSource, sdp, server::Server, session};

pub use crate::server::Response;

#[cfg(feature = "server")]
#[derive(Clone)]
struct RouterState {
	server: Server,
	subscriber: moq_net::origin::Consumer,
	/// Watches the whole subscribe scope while the router lives, so a lazily discovering
	/// client session asks its peer once rather than once per offered path, which an
	/// unauthenticated caller could otherwise name without bound.
	_watch: std::sync::Arc<moq_net::announce::Consumer>,
}

/// How long WHEP negotiation waits for the broadcast's first catalog snapshot
/// before failing the request. A broadcast can be announced (or served by a
/// dynamic origin fallback) yet never publish a catalog; without a bound the
/// `accept` future parks forever inside the HTTP handler, leaking the request.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(5);

/// Build the WHEP axum router. Only with the `server` feature.
#[cfg(feature = "server")]
pub(crate) fn router(server: Server, subscriber: moq_net::origin::Consumer) -> Router {
	Router::new()
		.route("/{*path}", post(handle).delete(delete))
		.with_state(RouterState {
			_watch: std::sync::Arc::new(subscriber.clone().with_hidden(true).announced()),
			server,
			subscriber,
		})
}

#[cfg(feature = "server")]
async fn handle(
	state: State<RouterState>,
	path: Path<String>,
	OriginalUri(uri): OriginalUri,
	headers: HeaderMap,
	body: Bytes,
) -> HttpResponse {
	let (state, path) = (state.0, path.0);
	match accept_offer(&state.server, &state.subscriber, &path, &headers, body).await {
		Ok(response) => {
			let Response {
				resource_id,
				answer,
				session,
				..
			} = response;
			let mut response_headers = HeaderMap::new();
			response_headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/sdp"));
			if let Some(loc) = crate::server::session_location(&uri, &resource_id) {
				response_headers.insert(header::LOCATION, loc);
			}
			tokio::spawn(async move {
				let _ = session.run().await;
			});
			(StatusCode::CREATED, response_headers, answer).into_response()
		}
		Err(err) => {
			tracing::warn!(%err, "whep request failed");
			(status_for(&err), err.to_string()).into_response()
		}
	}
}

/// Router glue: enforce the WHEP `Content-Type` then hand the raw offer to
/// [`accept`], using the request path as the (unauthenticated) broadcast name.
#[cfg(feature = "server")]
async fn accept_offer(
	server: &Server,
	subscriber: &moq_net::origin::Consumer,
	path: &str,
	headers: &HeaderMap,
	body: Bytes,
) -> Result<Response> {
	if !is_sdp(headers) {
		return Err(Error::InvalidSdp("expected Content-Type: application/sdp".into()));
	}
	let offer = std::str::from_utf8(&body).map_err(|err| Error::InvalidSdp(err.to_string()))?;
	accept(
		server,
		subscriber,
		moq_net::Path::new(path).map_err(moq_net::Error::from)?,
		offer,
	)
	.await
}

#[cfg(feature = "server")]
async fn delete(State(state): State<RouterState>, Path(path): Path<String>) -> StatusCode {
	crate::server::delete(&state.server, &path)
}

/// Accept a WHEP SDP offer and egress the MoQ broadcast `broadcast` (a path
/// relative to `subscriber`'s root) to the negotiated WebRTC peer.
///
/// This is the negotiation core behind [`Server::subscribe_router`], exposed so an embedder can own
/// the HTTP route and authentication: verify the request, resolve the authorized
/// broadcast name, scope `subscriber` to the caller's grants, then hand the raw
/// SDP offer here. Taking the consumer explicitly (rather than using the server's)
/// lets the embedder egress through a *scoped* origin, so the subscribe scope is
/// enforced by moq-net exactly as for a native session; the bundled [`Server::subscribe_router`]
/// passes the server's own (unauthenticated) consumer. It parses the offer,
/// resolves the broadcast on `subscriber`, restricts the answer to the codecs the
/// catalog actually has, registers a media session on the shared mux, and
/// returns the SDP answer plus an opaque `resource_id` for the WHEP `Location`
/// header. The caller must run the returned [`Response`] to drive the MoQ->RTP
/// session. Mirrors [`whip::accept`](super::whip::accept).
///
/// Each call watches `broadcast` while it resolves. A caller serving many paths from one
/// lazily discovering client session should hold an `announced()` stream over its scope,
/// as [`Server::subscribe_router`] does, so each call rides that one watch.
///
/// `offer` is the raw SDP body; the caller is responsible for checking the
/// `Content-Type: application/sdp` request header. Fails with [`Error::InvalidSdp`]
/// on a malformed offer, and surfaces a broadcast still unannounced after 5s (or one
/// outside `subscriber`'s scope) as [`Error::Moq`].
pub async fn accept(
	server: &Server,
	subscriber: &moq_net::origin::Consumer,
	broadcast: impl Into<moq_net::Path>,
	offer: &str,
) -> Result<Response> {
	let offer = sdp::parse_offer(offer)?;

	let source = resolve(subscriber, broadcast.into()).await?;
	let codecs = source.catalog_codecs();
	if codecs.is_empty() {
		return Err(Error::NoRenditions);
	}

	// Register a session on the shared media mux (see whip::accept). Restrict our
	// CodecConfig before accept_offer so the answer intersects the peer's offer
	// with what the catalog actually has, instead of agreeing to a codec we can't
	// fulfil; set the mux's known ICE credentials on the same config.
	let mux = server.mux().await?;
	let (creds, inbound, registration) = mux.register();
	let mut rtc = session::rtc_config_with_codecs(&codecs)
		.set_local_ice_credentials(creds)
		.build(moq_clock::now().into_std());
	for addr in mux.candidates() {
		let cand = Candidate::host(*addr, "udp").map_err(Error::rtc)?;
		rtc.add_local_candidate(cand);
	}

	let answer = rtc.sdp_api().accept_offer(offer).map_err(Error::rtc)?;
	let resource_id = sdp::new_resource_id();
	let socket = moq_ice::Socket::new(mux.socket(), mux.candidates().to_vec(), inbound)?;
	let session = session::Session::egress(rtc, socket, source)?;

	// Register before returning so a DELETE that races startup still finds the
	// session; Response::run unregisters itself when it ends.
	let cancel = server.register_session(resource_id.clone());

	Ok(Response {
		resource_id: resource_id.clone(),
		answer: sdp::render_answer(&answer),
		session: crate::server::AcceptedSession {
			server: server.clone(),
			resource_id,
			session: Some(session),
			registration: Some(registration),
			cancel: Some(cancel),
			role: "whep server",
		},
	})
}

#[cfg(feature = "server")]
fn is_sdp(headers: &HeaderMap) -> bool {
	headers
		.get(header::CONTENT_TYPE)
		.and_then(|v| v.to_str().ok())
		.map(|v| v.eq_ignore_ascii_case("application/sdp"))
		.unwrap_or(false)
}

/// Resolve `path` on the subscribe origin and wait for its first catalog.
///
/// Following the path watches it while it resolves, which is what makes a lazily
/// discovering client session ask its peer about it, and waits out a publisher that has not
/// announced yet. The `Source` resolves any sibling broadcast a rendition's catalog `broadcast` field
/// references (e.g. `./source`) through the same origin.
///
/// Both waits share one bound, or an unannounced broadcast, or an announced-but-catalog-less
/// one, would park the handler forever. No route by the deadline is
/// [`moq_net::Error::Unroutable`], so the WHEP client sees a 404 and retries (typical).
async fn resolve(subscriber: &moq_net::origin::Consumer, path: moq_net::Path) -> Result<EgressSource> {
	let deadline = moq_clock::now() + CATALOG_TIMEOUT;
	let mut following = subscriber.follow(path, None)?;
	let broadcast = moq_clock::timeout_at(deadline, following.available())
		.await
		.map_err(|_| Error::Moq(moq_net::Error::Unroutable))??;
	moq_clock::timeout_at(
		deadline,
		EgressSource::new(moq_mux::Source::new(subscriber.clone(), broadcast)),
	)
	.await
	.map_err(|_| Error::CatalogTimeout)?
}

#[cfg(feature = "server")]
fn status_for(err: &Error) -> StatusCode {
	match err {
		Error::InvalidSdp(_) | Error::Moq(moq_net::Error::EmptySegment) => StatusCode::BAD_REQUEST,
		Error::UnsupportedCodec(_) => StatusCode::UNSUPPORTED_MEDIA_TYPE,
		Error::SessionNotFound => StatusCode::NOT_FOUND,
		Error::Moq(moq_net::Error::Unauthorized) => StatusCode::UNAUTHORIZED,
		Error::Moq(moq_net::Error::NotFound | moq_net::Error::Unroutable) => StatusCode::NOT_FOUND,
		_ => StatusCode::INTERNAL_SERVER_ERROR,
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use bytes::Bytes;

	use crate::codec::{Bridge, Frame};

	const TIMEOUT: Duration = Duration::from_secs(10);

	/// A plain (lazily discovering) moq-lite client subscribing from a server that
	/// publishes `pub_origin`, over loopback TCP.
	async fn lazy_client(
		pub_origin: moq_net::origin::Producer,
	) -> (
		moq_net::origin::Producer,
		moq_tokio::Connection,
		tokio::task::JoinHandle<()>,
	) {
		let sub_origin = moq_tokio::origin::spawn();

		let mut listen = moq_tokio::listen::Config::default();
		listen.tcp.bind = Some("127.0.0.1:0".parse().expect("addr"));
		let mut server = listen
			.init(Default::default())
			.expect("server")
			.listen()
			.await
			.expect("listen");
		let addr = server.tcp_local_addr().expect("tcp listener bound");
		let accept = tokio::spawn(async move {
			let request = server.accept().await.expect("accept");
			let session = request
				.with_publisher(&pub_origin)
				.ok()
				.await
				.expect("publisher session");
			let _ = session.closed().await;
		});

		let client = moq_tokio::connect::Config::default()
			.init(Default::default())
			.expect("client")
			.with_subscriber(sub_origin.clone())
			.with_reconnect(false);
		let url: url::Url = format!("tcp://{addr}/").parse().expect("url");
		let connection = moq_clock::timeout(TIMEOUT, client.connect(url).established())
			.await
			.expect("connect timed out")
			.expect("connect");
		(sub_origin, connection, accept)
	}

	/// A plain moq-lite client origin asks its peer only about watched paths, so resolving
	/// a WHEP play must watch the broadcast rather than request it blind.
	#[tokio::test]
	async fn a_broadcast_resolves_through_a_lazy_client_session() {
		let pub_origin = moq_tokio::origin::spawn();

		let source = pub_origin.create_broadcast("cam").expect("create broadcast");
		source
			.announce(moq_net::origin::Route::default())
			.expect("announce broadcast");
		let catalog =
			moq_mux::catalog::Producer::new(&mut source.broadcast().clone(), moq_mux::catalog::Config::default())
				.expect("create catalog");
		let mut opus = crate::codec::opus::Bridge::new(source.broadcast().clone(), catalog, 48_000, 2)
			.expect("create Opus bridge");
		opus.push(Frame {
			timestamp_us: 20_000,
			payload: Bytes::from_static(&[0xfc, 0xff, 0xfe]),
		})
		.expect("publish packet");

		let (sub_origin, _connection, accept) = lazy_client(pub_origin).await;

		let resolved = super::resolve(&sub_origin.consume(), moq_net::Path::from_static("cam"))
			.await
			.expect("the broadcast resolves");
		assert!(!resolved.catalog_codecs().is_empty());

		accept.abort();
	}

	/// The router watches its whole subscribe scope, so an offer's lookup rides one
	/// request to the peer rather than opening its own for every path a caller names.
	#[cfg(feature = "server")]
	#[tokio::test]
	async fn offers_ride_the_routers_scope_watch() {
		let pub_origin = moq_tokio::origin::spawn();
		let other = pub_origin.create_broadcast("other").expect("create broadcast");
		other
			.announce(moq_net::origin::Route::default())
			.expect("announce broadcast");
		let (sub_origin, _connection, accept) = lazy_client(pub_origin).await;

		let _router = super::router(crate::server::Server::new(Default::default()), sub_origin.consume());
		let missing = moq_clock::timeout(TIMEOUT, sub_origin.consume().request("missing", None))
			.await
			.expect("sync timed out");
		assert!(
			matches!(missing, Err(moq_net::Error::Unroutable)),
			"the missing path resolved"
		);

		// Nothing watches `other` on its own, yet the synced lookup already discovered it.
		sub_origin
			.consume()
			.request("other", None)
			.await
			.expect("discovered through the scope watch");

		drop(other);
		accept.abort();
	}
}
