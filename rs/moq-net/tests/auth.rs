//! In-band AUTH over the in-memory mock transport: both sides learn their grant,
//! tokens union, and a publication outside the grant fails loud. Every case runs on
//! moq-lite-06 and on moq-transport with the MoQ Auth extension.

mod support;

use std::time::Duration;

use moq_net::{
	Client, Error, Hop, Pattern, Patterns, Server, Session, SessionError, StreamError, Version,
	auth::{self, Grant},
	origin,
};
use support::harness::{now, run};
use support::mock::{MockSession, create_mock_session_pair};

/// Maximum time any single test may run before being treated as a deadlock.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

const LITE_06: &str = "moq-lite-06";
/// The first draft that negotiates MoQ Auth, and the newest.
const MOQT_17: &str = "moq-transport-17";
/// A draft at the deployed floor, used for the request-token launch shape.
const MOQT_18: &str = "moq-transport-18";
const MOQT_19: &str = "moq-transport-19";
const MOQT_22: &str = "moq-transport-22";

/// Run each case on every version that exchanges AUTH.
macro_rules! cases {
	($($case:ident),* $(,)?) => {
		mod lite_06 {
			$(#[tokio::test] async fn $case() { super::$case(super::LITE_06).await })*
		}
		mod moqt_17 {
			$(#[tokio::test] async fn $case() { super::$case(super::MOQT_17).await })*
		}
		mod moqt_22 {
			$(#[tokio::test] async fn $case() { super::$case(super::MOQT_22).await })*
		}
	};
}

cases!(
	both_sides_learn_their_grant_from_scoped_origins,
	a_publish_only_session_grants_no_subscribe,
	an_out_of_scope_announce_aborts_with_the_path,
	a_broadcast_published_before_the_grant_is_checked,
	an_unanswered_token_does_not_suspend_the_check,
	tokens_union_and_withdrawing_one_shrinks_it,
	an_update_replaces_one_tokens_grant,
	a_revoked_grant_withdraws_and_can_be_restored,
	a_refused_token_reports_the_code,
	a_refused_setup_token_grants_nothing,
	dropping_the_requests_refuses_queued_tokens,
	a_closed_session_holds_no_grant,
	an_issued_grant_ends_with_its_session,
	a_reset_auth_stream_reports_unsupported,
	a_revoked_grant_cancels_its_subscriptions,
	nothing_outside_the_grant_reaches_the_peer,
);

/// Run each case on moq-transport alone, whose namespace prefixes cannot carry every
/// pattern.
macro_rules! prefix_cases {
	($($case:ident),* $(,)?) => {
		mod moqt_17_prefixes {
			$(#[tokio::test] async fn $case() { super::$case(super::MOQT_17).await })*
		}
		mod moqt_22_prefixes {
			$(#[tokio::test] async fn $case() { super::$case(super::MOQT_22).await })*
		}
	};
}

prefix_cases!(
	an_unrepresentable_grant_is_unsupported,
	an_unrepresentable_update_revokes_only_its_token,
	a_grant_too_large_for_one_message_is_unsupported,
);

#[tokio::test]
async fn lite_06_carries_pattern_grants() {
	pattern_grants_arrive_exactly(LITE_06).await
}

#[tokio::test]
async fn lite_06_enforces_a_wildcard_grant() {
	a_wildcard_grant_is_enforced(LITE_06).await
}

#[tokio::test]
async fn lite_05_has_no_grant() {
	older_versions_have_no_grant("moq-lite-05").await
}

#[tokio::test]
async fn moqt_16_has_no_grant() {
	older_versions_have_no_grant("moq-transport-16").await
}

/// Build an origin producer, spawning its driver on the ambient runtime.
fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	tokio::spawn(run(driver));
	producer
}

fn patterns(prefixes: &[&str]) -> Patterns {
	prefixes
		.iter()
		.map(|prefix| Pattern::subtree(prefix).unwrap())
		.collect()
}

fn grant(publish: &[&str], subscribe: &[&str]) -> Grant {
	Grant {
		publish: patterns(publish),
		subscribe: patterns(subscribe),
		expires: None,
	}
}

/// Wait for a watch to hold a grant satisfying `f`.
async fn wait_for(mut watch: auth::Watch, f: impl Fn(&Option<Grant>) -> bool) -> Option<Grant> {
	loop {
		let current = watch.peek();
		if f(&current) {
			return current;
		}
		watch.changed().await.expect("grant watch ended");
	}
}

/// The union, once the peer has answered anything.
async fn granted(session: &Session) -> Grant {
	wait_for(session.auth().grant(), Option::is_some).await.unwrap()
}

/// Wait until `path` is announced (`true`) or retracted (`false`) in `origin`.
async fn wait_announced(origin: &origin::Consumer, path: &str, active: bool) {
	let mut announced = origin.announced();
	let mut live = std::collections::HashSet::new();
	let apply = |live: &mut std::collections::HashSet<String>, update: moq_net::announce::Update| {
		match update.kind.is_active() {
			true => live.insert(update.prefix.to_string()),
			false => live.remove(update.prefix.as_str()),
		};
	};
	// Take in the replay first, so a retraction is judged against what is announced now
	// rather than against an empty start.
	while let Some(update) = futures::FutureExt::now_or_never(announced.next()).flatten() {
		apply(&mut live, update);
	}
	loop {
		if live.contains(path) == active {
			return;
		}
		apply(&mut live, announced.next().await.expect("origin closed"));
	}
}

#[derive(Default)]
struct Options {
	client_publish: Option<origin::Producer>,
	client_subscribe: Option<origin::Producer>,
	server_publish: Option<origin::Producer>,
	server_subscribe: Option<origin::Producer>,
	/// Take the server's AUTH requests before its driver runs.
	server_requests: bool,
	/// A request token the client attaches to its outgoing PUBLISH_NAMESPACE / SUBSCRIBE.
	client_request_token: Option<Vec<u8>>,
	/// The client does not offer the MoQ Auth extension (`Extensions::auth` off).
	client_decline_auth: bool,
	/// The server does not offer the MoQ Solicit extension (`Extensions::solicit` off).
	server_decline_solicit: bool,
	/// The server does not offer the MoQ Auth extension (`Extensions::auth` off).
	server_decline_auth: bool,
	version: Option<&'static str>,
}

struct Pair {
	client: Session,
	server: Session,
	client_transport: MockSession,
	server_transport: MockSession,
	requests: Option<auth::Requests>,
	/// Aborting it drops the server's driver without letting it finish.
	server_driver: tokio::task::AbortHandle,
}

async fn connect(opts: Options) -> Pair {
	let version: Version = opts.version.unwrap_or(LITE_06).parse().unwrap();
	let (client_transport, server_transport) = create_mock_session_pair(Some(version.alpn()));

	let mut client = Client::new().with_versions(version.into());
	if let Some(publish) = &opts.client_publish {
		client = client.with_publisher(publish);
	}
	if let Some(subscribe) = opts.client_subscribe {
		client = client.with_subscriber(subscribe);
	}
	if opts.client_decline_auth {
		let mut extensions = moq_net::setup::Extensions::default();
		extensions.auth = false;
		client = client.with_extensions(extensions);
	}

	let mut server = Server::new().with_versions(version.into());
	if opts.server_decline_solicit || opts.server_decline_auth {
		let mut extensions = moq_net::setup::Extensions::default();
		extensions.solicit = !opts.server_decline_solicit;
		extensions.auth = !opts.server_decline_auth;
		server = server.with_extensions(extensions);
	}
	if let Some(publish) = &opts.server_publish {
		server = server.with_publisher(publish);
	}
	if let Some(subscribe) = opts.server_subscribe {
		server = server.with_subscriber(subscribe);
	}

	let observe = client_transport.clone();
	let observe_server = server_transport.clone();
	let client_token = opts.client_request_token;
	let client_fut = async {
		let (session, driver) = client.connect(now(), client_transport).await.expect("client handshake");
		// Set before the driver runs, so the first request already carries it.
		if let Some(token) = client_token {
			session.auth().set_request_token(token);
		}
		tokio::spawn(run(driver));
		session
	};
	let server_fut = async {
		let handshake = server
			.accept_request(now(), server_transport)
			.await
			.expect("server handshake");
		// Taken before the session starts, the way a relay verifying tokens would.
		let requests = opts
			.server_requests
			.then(|| handshake.auth().requests().expect("requests available before ok()"));
		let (session, driver) = handshake.ok().await.expect("server accept");
		let driver = tokio::spawn(run(driver)).abort_handle();
		(session, requests, driver)
	};
	let (client, (server, requests, server_driver)) = tokio::join!(client_fut, server_fut);

	Pair {
		client,
		server,
		client_transport: observe,
		server_transport: observe_server,
		requests,
		server_driver,
	}
}

/// Answer the peer's tokens from a table, holding every grant (and any request the
/// table leaves unanswered) for as long as the returned task lives.
fn serve(
	mut requests: auth::Requests,
	answer: impl Fn(&[u8]) -> Option<Grant> + Send + 'static,
) -> tokio::sync::mpsc::UnboundedReceiver<(Vec<u8>, auth::Issued)> {
	let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
	tokio::spawn(async move {
		let mut unanswered = Vec::new();
		while let Some(request) = requests.next().await {
			let token = request.token().to_vec();
			match answer(&token) {
				Some(grant) => {
					let _ = tx.send((token, request.accept(grant)));
				}
				None => unanswered.push(request),
			}
		}
	});
	rx
}

fn within<F: std::future::Future>(f: F) -> tokio::time::Timeout<F> {
	tokio::time::timeout(TEST_TIMEOUT, f)
}

/// Each side's default grant is what the other side's origin handles allow:
/// its subscribe half bounds what we may publish, its publish half what we may
/// subscribe to.
async fn both_sides_learn_their_grant_from_scoped_origins(version: &'static str) {
	within(async {
		let relay = produce_origin(1);
		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			client_subscribe: Some(produce_origin(3)),
			server_publish: Some(relay.scope("", &patterns(&["room"])).unwrap()),
			server_subscribe: Some(relay.scope("", &patterns(&["room/alice"])).unwrap()),
			..Default::default()
		})
		.await;

		assert_eq!(granted(&pair.client).await, grant(&["room/alice"], &["room"]));
		// The empty prefix: an unscoped origin grants everything.
		assert_eq!(granted(&pair.server).await, grant(&[""], &[""]));
	})
	.await
	.expect("timed out");
}

/// A missing half grants nothing: the empty list, distinct from the empty prefix.
async fn a_publish_only_session_grants_no_subscribe(version: &'static str) {
	within(async {
		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			..Default::default()
		})
		.await;

		// The server may subscribe to what the client publishes, but publish nothing to
		// a client that never reads.
		assert_eq!(granted(&pair.server).await, grant(&[], &[""]));
		assert_eq!(granted(&pair.client).await, grant(&[""], &[]));
	})
	.await
	.expect("timed out");
}

/// A broadcast outside the grant aborts the session and names the path, where it
/// used to wait forever for a solicitation that never comes.
async fn an_out_of_scope_announce_aborts_with_the_path(version: &'static str) {
	within(async {
		let publisher = produce_origin(2);
		let relay = produce_origin(1);
		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(publisher.clone()),
			server_subscribe: Some(relay.scope("", &patterns(&["baz"])).unwrap()),
			..Default::default()
		})
		.await;

		// In scope: served, and nothing aborts.
		let ok = publisher.create_broadcast("baz/ok").unwrap();
		ok.announce(Default::default()).unwrap();
		wait_announced(&relay.consume(), "baz/ok", true).await;
		assert_eq!(pair.client_transport.close_reason(), None);

		let bad = publisher.create_broadcast("foo/bar").unwrap();
		bad.announce(Default::default()).unwrap();
		let err = pair.server.closed().await;
		assert!(
			matches!(err, Error::Session(SessionError::Unauthorized)),
			"server saw {err:?}"
		);
		let (code, reason) = pair.client_transport.close_reason().expect("closed");
		assert_eq!(code, SessionError::Unauthorized.to_code());
		assert_eq!(reason, "unauthorized: foo/bar");
	})
	.await
	.expect("timed out");
}

/// A request token on a PUBLISH_NAMESPACE authorizes the announce on a session that never
/// negotiated the MoQ Auth extension (draft-14), the standard moq-transport peer shape, when the
/// token reaches the acceptor THROUGH THE DRIVER rather than an inline `verify_request`.
///
/// This exercises `ietf::start`'s legacy (draft 14-16) branch. That branch built its
/// Subscriber without `.with_auth(auth)`, so the driver's subscriber consulted a fresh
/// default `Handle` instead of the session handle the `requests()` acceptor was installed on:
/// `verify_request` found no `App` acceptor and refused the announce `NOT_SUPPORTED`, and the
/// announce never reached the server origin. The modern (17+) branch already wired
/// `.with_auth`, so only the legacy / no-extension path was affected, and the unit tests
/// missed it by constructing the Subscriber with `.with_auth` by hand.
#[tokio::test]
async fn a_request_token_authorizes_a_legacy_announce_through_the_driver() {
	within(async {
		let publisher = produce_origin(2);
		let relay = produce_origin(1);
		let mut pair = connect(Options {
			version: Some("moq-transport-14"),
			client_publish: Some(publisher.clone()),
			// USE_VALUE (0x03), token kind 0, value "ok": a decodable request token the
			// acceptor answers unconditionally below.
			client_request_token: Some(vec![0x03, 0x00, b'o', b'k']),
			server_subscribe: Some(relay.scope("", &patterns(&["room/alice"])).unwrap()),
			server_requests: true,
			..Default::default()
		})
		.await;

		// The server answers the request token from its acceptor with a grant covering the
		// announced path. Held for the test by the returned receiver.
		let requests = pair.requests.take().expect("server took its requests pre-ok");
		let mut answered = serve(requests, |_token| Some(grant(&["room/alice"], &["room/alice"])));

		// The client announces under the token. On the legacy path there is no session grant,
		// so the token is the only authorization for the announce.
		let bc = publisher.create_broadcast("room/alice").unwrap();
		bc.announce(Default::default()).unwrap();

		// Mechanism: the token reached the acceptor over the driver (not admitted by a
		// permissive default, and not refused NOT_SUPPORTED by a disconnected handle).
		let (_token, _issued) = answered.recv().await.expect("the token reached the acceptor");

		// End to end: the verified announce reached the server's subscribe origin.
		wait_announced(&relay.consume(), "room/alice", true).await;
	})
	.await
	.expect("timed out");
}

/// A client may decline the MoQ Auth extension, connecting at draft-18 as a peer that
/// does not negotiate it (a non-moq-dev encoder or CDN). Its SETUP omits the option, so
/// the server sees `declared.auth == false` and the session carries no connection grant
/// (`None` union). A request-borne `AUTHORIZATION TOKEN` is then the authorizing artifact
/// and reaches the server's acceptor, where a normal draft-18 client's connection grant
/// would cover the request and skip the token. A token-less request on such a
/// session is admitted by the permissive default of the ungranted session.
///
/// The token is exercised on a SUBSCRIBE, which is always a request and carries the token
/// on every draft. A request token on a PUBLISH_NAMESPACE does NOT reach a moq-net peer at
/// draft-16+ regardless of this option: moq-net declares MoQ Solicit unconditionally, so
/// the announce answers the peer's SUBSCRIBE_NAMESPACE inline via `ietf::Namespace`, which
/// carries no token, and the token-bearing unsolicited PUBLISH_NAMESPACE loop is disabled.
/// A server that does not offer the MoQ Auth extension leaves it un-negotiated even with a
/// client that offers it: neither side presents a session token.
#[tokio::test]
async fn a_server_may_decline_the_auth_extension() {
	within(async {
		// Control: with the default extensions the client's connection credential earns a grant.
		let offered = connect(Options {
			version: Some(MOQT_18),
			..Default::default()
		})
		.await;
		wait_for(offered.client.auth().grant(), Option::is_some).await.unwrap();

		let declined = connect(Options {
			version: Some(MOQT_18),
			server_decline_auth: true,
			..Default::default()
		})
		.await;
		let granted = tokio::time::timeout(
			Duration::from_millis(200),
			wait_for(declined.client.auth().grant(), Option::is_some),
		)
		.await;
		assert!(granted.is_err(), "no grant without the extension: {granted:?}");
		assert!(matches!(declined.client.auth().add("x").await, Err(Error::Unsupported)));
	})
	.await
	.expect("timed out");
}

#[tokio::test]
async fn a_client_may_decline_the_auth_extension() {
	within(async {
		let ts = |ms| moq_net::Timestamp::from_millis(ms).unwrap();
		let prefs = || moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10));
		let server_origin = produce_origin(1);
		let down = server_origin.create_broadcast("room/alice").unwrap();
		let down_track = down.create_track("video", None).unwrap();
		down.announce(Default::default()).unwrap();

		let received = produce_origin(3);
		let mut pair = connect(Options {
			version: Some(MOQT_18),
			client_subscribe: Some(received.clone()),
			// USE_VALUE (0x03), token kind 0, value "ok".
			client_request_token: Some(vec![0x03, 0x00, b'o', b'k']),
			client_decline_auth: true,
			server_publish: Some(server_origin.clone()),
			server_requests: true,
			..Default::default()
		})
		.await;

		// declared.auth == false: the declining client speaks no AUTH, so it holds no
		// session grant and cannot present a session token (unlike a normal draft-18 peer).
		assert_eq!(pair.client.auth().grant().peek(), None);
		assert!(matches!(pair.client.auth().add("x").await, Err(Error::Unsupported)));

		// The client's token-bearing SUBSCRIBE reaches the acceptor: on the `None`-union
		// session the covers-gate does not short-circuit, so the token is verified rather
		// than admitted by a covering connection grant.
		let requests = pair.requests.take().expect("server took its requests pre-ok");
		let mut answered = serve(requests, |_token| Some(grant(&["room/alice"], &["room/alice"])));

		let remote = received.consume().routed_broadcast("room/alice").await.unwrap();
		let mut sub = remote.track("video").unwrap().subscribe(prefs()).await.unwrap();
		let mut group = down_track.append_group().unwrap();
		group.write_frame(ts(0), b"down".as_ref()).unwrap();

		let (_token, _issued) = answered.recv().await.expect("the token reached the acceptor");
		sub.recv_group().await.unwrap().unwrap();

		// Token-less: a declining client with no token is admitted by the permissive default.
		let bare_publisher = produce_origin(4);
		let bare_relay = produce_origin(5);
		let bare = connect(Options {
			version: Some(MOQT_18),
			client_publish: Some(bare_publisher.clone()),
			client_decline_auth: true,
			server_subscribe: Some(bare_relay.clone()),
			..Default::default()
		})
		.await;
		let bc = bare_publisher.create_broadcast("room/bob").unwrap();
		bc.announce(Default::default()).unwrap();
		wait_announced(&bare_relay.consume(), "room/bob", true).await;
		assert_eq!(bare.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// A request token renews over the wire: replacing it on the client's `Session::auth()`
/// re-presents it on the live SUBSCRIBE as a REQUEST_UPDATE, the server's driver routes it to
/// the acceptor, and the new grant keeps the subscription alive past the old one's expiry.
/// Runs through both drivers at draft-18, in the shape a base moq-transport peer produces
/// (no MoQ Auth, so the token is what authorizes).
#[tokio::test]
async fn a_request_token_renews_a_subscription_through_the_driver() {
	within(async {
		let ts = |ms| moq_net::Timestamp::from_millis(ms).unwrap();
		let prefs = || moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10));
		let server_origin = produce_origin(1);
		let down = server_origin.create_broadcast("room/alice").unwrap();
		let down_track = down.create_track("video", None).unwrap();
		down.announce(Default::default()).unwrap();

		let first = vec![0x03, 0x00, b'a', b'a'];
		let second = vec![0x03, 0x00, b'b', b'b'];
		let received = produce_origin(3);
		let mut pair = connect(Options {
			version: Some(MOQT_18),
			client_subscribe: Some(received.clone()),
			client_request_token: Some(first.clone()),
			client_decline_auth: true,
			server_publish: Some(server_origin.clone()),
			server_requests: true,
			..Default::default()
		})
		.await;

		// The first token lapses in a second; the renewal never does. Real time, not a paused
		// clock: both drivers and the mock transport run on their own tasks.
		let expires = Some(now() + Duration::from_secs(1));
		// The acceptor sees the Token structure's value, past its USE_VALUE header.
		let renewal = second[2..].to_vec();
		let requests = pair.requests.take().expect("server took its requests pre-ok");
		let mut answered = serve(requests, move |token| {
			let mut granted = grant(&[], &["room/alice"]);
			if token != renewal.as_slice() {
				granted.expires = expires;
			}
			Some(granted)
		});

		let remote = received.consume().routed_broadcast("room/alice").await.unwrap();
		let mut sub = remote.track("video").unwrap().subscribe(prefs()).await.unwrap();
		let mut group = down_track.append_group().unwrap();
		group.write_frame(ts(0), b"one".as_ref()).unwrap();
		group.finish().unwrap();
		let (token, _first_issued) = answered.recv().await.expect("the first token reached the acceptor");
		assert_eq!(token, first[2..]);
		sub.recv_group().await.unwrap().unwrap();

		pair.client.auth().set_request_token(second.clone());
		let (token, _renewed) = answered.recv().await.expect("the renewal reached the acceptor");
		assert_eq!(token, second[2..], "the replaced token rides the REQUEST_UPDATE");

		// Past the first grant's expiry the subscription still delivers.
		tokio::time::sleep(Duration::from_millis(1500)).await;
		let mut group = down_track.append_group().unwrap();
		group.write_frame(ts(2000), b"two".as_ref()).unwrap();
		group.finish().unwrap();
		sub.recv_group()
			.await
			.unwrap()
			.expect("the renewed subscription is still live");
		assert_eq!(
			pair.client_transport.close_reason(),
			None,
			"renewal never touches the session"
		);
		// The same subscription carried on: a lapse would have ended it, and the client's
		// re-subscribe would have reached the acceptor as another request.
		assert!(
			answered.try_recv().is_err(),
			"the original subscription was renewed, not replaced"
		);
	})
	.await
	.expect("timed out");
}

/// The sender honors the receiver's MAX_REQUEST_UPDATES credit: it keeps at most one
/// renewal in flight per request and coalesces replacements that arrive while one is
/// unanswered, so a burst of token changes never outruns the credit (draft-19 section
/// 10.3.1.7).
///
/// Two drivers over the in-process transport at draft-19, where the serving side advertises
/// and enforces a 16-update credit with a session close ([`SessionError::TooManyRequestUpdates`]).
/// The acceptor holds the first renewal's verifier, the client replaces its token well past
/// the credit, and the test asserts the connection stays up, only the held renewal reaches the
/// acceptor, and resolving it releases exactly the newest token, not any coalesced between.
///
/// A fire-and-forget sender would put every renewal outstanding behind the held verifier and
/// the serving side would close the session, losing every request on it.
///
/// [`SessionError::TooManyRequestUpdates`]: moq_net::SessionError::TooManyRequestUpdates
#[tokio::test]
async fn a_held_renewal_coalesces_a_burst_without_tripping_the_credit() {
	within(async {
		let ts = |ms| moq_net::Timestamp::from_millis(ms).unwrap();
		let prefs = || moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10));
		let server_origin = produce_origin(1);
		let down = server_origin.create_broadcast("room/alice").unwrap();
		let down_track = down.create_track("video", None).unwrap();
		down.announce(Default::default()).unwrap();

		// USE_VALUE (0x03), token kind 0, then a distinct value per credential.
		let token = |n: u8| vec![0x03, 0x00, b't', n];
		let initial = token(0);
		let received = produce_origin(3);
		let mut pair = connect(Options {
			version: Some(MOQT_19),
			client_subscribe: Some(received.clone()),
			client_request_token: Some(initial.clone()),
			client_decline_auth: true,
			server_publish: Some(server_origin.clone()),
			server_requests: true,
			..Default::default()
		})
		.await;

		// The acceptor runs concurrently with the subscribe (the initial token rides the
		// SUBSCRIBE, so nothing reaches it until we subscribe). It reports every token it
		// verifies and holds the first renewal's verifier until released, the way a relay with
		// a slow authorizer would.
		let mut requests = pair.requests.take().expect("server took its requests pre-ok");
		let (seen_tx, mut seen) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
		let release = std::sync::Arc::new(tokio::sync::Notify::new());
		let release_waiter = release.clone();
		let acceptor = tokio::spawn(async move {
			let mut issued = Vec::new();
			let mut count = 0u64;
			while let Some(request) = requests.next().await {
				let token = request.token().to_vec();
				count += 1;
				// Request 1 is the initial SUBSCRIBE token; request 2 is the first renewal,
				// held until the test releases it.
				if count == 2 {
					seen_tx.send(token).ok();
					release_waiter.notified().await;
				} else {
					seen_tx.send(token).ok();
				}
				issued.push(request.accept(grant(&[], &["room/alice"])));
			}
		});

		// Establish and deliver one group so the subscription is live. The initial token rides
		// the SUBSCRIBE, which the acceptor (above) verifies concurrently.
		let remote = received.consume().routed_broadcast("room/alice").await.unwrap();
		let mut sub = remote.track("video").unwrap().subscribe(prefs()).await.unwrap();
		let mut group = down_track.append_group().unwrap();
		group.write_frame(ts(0), b"one".as_ref()).unwrap();
		group.finish().unwrap();
		sub.recv_group().await.unwrap().unwrap();
		assert_eq!(
			seen.recv().await.expect("initial token"),
			initial[2..],
			"the SUBSCRIBE carries the token"
		);

		// Replace the token once and let that one renewal reach the held acceptor, so the held
		// renewal is deterministic regardless of scheduling.
		pair.client.auth().set_request_token(token(1));
		assert_eq!(
			seen.recv().await.expect("first renewal"),
			token(1)[2..],
			"the first replacement goes out immediately"
		);

		// With that renewal held unanswered, replace the token many more times, spaced so the
		// driver observes each: the scenario the credit guards. Far past any plausible credit,
		// so the test keeps guarding if MAX_REQUEST_UPDATES grows. A fire-and-forget sender
		// would put all of these outstanding behind the held verifier and the serving side
		// would close the session with TOO_MANY_REQUEST_UPDATES, losing every request on it;
		// the one-in-flight rule coalesces them behind the held one instead.
		for n in 2..=64u8 {
			pair.client.auth().set_request_token(token(n));
			tokio::time::sleep(Duration::from_millis(5)).await;
		}

		assert_eq!(
			pair.client_transport.close_reason(),
			None,
			"the burst never tripped the receiver's credit"
		);
		assert_eq!(
			pair.server_transport.close_reason(),
			None,
			"the server never closed the session"
		);

		// Resolve the held verifier. The sender sends the coalesced renewal carrying the newest
		// token, not any of the ones replaced between.
		release.notify_one();
		assert_eq!(
			seen.recv().await.expect("coalesced renewal"),
			token(64)[2..],
			"the newest token wins; the rest are coalesced away"
		);

		// Only the newest coalesced renewal followed the held one: the burst collapsed to one,
		// not a backlog queued behind it.
		assert!(
			tokio::time::timeout(Duration::from_millis(100), seen.recv())
				.await
				.is_err(),
			"only the newest coalesced renewal followed, not a backlog"
		);
		assert_eq!(
			pair.client_transport.close_reason(),
			None,
			"the session stayed up throughout"
		);
		acceptor.abort();
	})
	.await
	.expect("timed out");
}

/// A server may decline the MoQ Solicit extension, so a peer sends an unsolicited
/// PUBLISH_NAMESPACE (the base moq-transport behavior) instead of answering our
/// SUBSCRIBE_NAMESPACE inline. Only the unsolicited PUBLISH_NAMESPACE carries an
/// `AUTHORIZATION TOKEN`; the inline `Namespace` entry has no parameter slot for one. So a
/// request-borne token on an announce reaches the acceptor exactly when the server does not
/// solicit, which is the shape a standard moq-transport peer (an encoder or CDN) always sends.
///
/// This runs at draft-18 (the deployed floor) in the launch shape: the client also declines
/// the AUTH extension, so the session's union is `None`, the covers-gate does not short-circuit
/// on a connection grant, and the request token is the authorizing artifact. The
/// control half shows the default: with Solicit declared the client answers inline, no token
/// reaches the acceptor, and the announce is admitted by the permissive default of the
/// ungranted session.
#[tokio::test]
async fn a_server_that_declines_solicit_gets_a_token_bearing_unsolicited_announce() {
	within(async {
		// USE_VALUE (0x03), token kind 0, value "ok".
		let request_token = vec![0x03, 0x00, b'o', b'k'];

		// Server declines Solicit: the client sends an unsolicited PUBLISH_NAMESPACE carrying
		// the token, which reaches the acceptor as a PublishNamespace request.
		let publisher = produce_origin(1);
		let relay = produce_origin(2);
		let mut pair = connect(Options {
			version: Some(MOQT_18),
			client_publish: Some(publisher.clone()),
			client_request_token: Some(request_token.clone()),
			client_decline_auth: true,
			server_subscribe: Some(relay.clone()),
			server_decline_solicit: true,
			server_requests: true,
			..Default::default()
		})
		.await;
		let bc = publisher.create_broadcast("room/alice").unwrap();
		bc.announce(Default::default()).unwrap();

		let requests = pair.requests.take().expect("server took its requests pre-ok");
		let mut answered = serve(requests, |_token| Some(grant(&["room/alice"], &["room/alice"])));
		// The announce is admitted once the token is granted.
		wait_announced(&relay.consume(), "room/alice", true).await;
		let (tok, _issued) = answered
			.recv()
			.await
			.expect("the unsolicited PUBLISH_NAMESPACE carried the token to the acceptor");
		assert_eq!(tok, b"ok", "the acceptor saw the request token's decoded value");

		// Control: with Solicit declared (the default) the same client answers our
		// SUBSCRIBE_NAMESPACE inline with a Namespace, which carries no token, so nothing
		// reaches the acceptor. The announce is still admitted by the permissive default.
		let publisher = produce_origin(3);
		let relay = produce_origin(4);
		let mut pair = connect(Options {
			version: Some(MOQT_18),
			client_publish: Some(publisher.clone()),
			client_request_token: Some(request_token.clone()),
			client_decline_auth: true,
			server_subscribe: Some(relay.clone()),
			// server_decline_solicit defaults false: the server declares Solicit.
			server_requests: true,
			..Default::default()
		})
		.await;
		let bc = publisher.create_broadcast("room/carol").unwrap();
		bc.announce(Default::default()).unwrap();

		let requests = pair.requests.take().expect("server took its requests pre-ok");
		let mut answered = serve(requests, |_token| Some(grant(&["room/carol"], &[])));
		wait_announced(&relay.consume(), "room/carol", true).await;
		assert!(
			answered.try_recv().is_err(),
			"a solicited (inline) announce carries no token, so the acceptor is never consulted"
		);
	})
	.await
	.expect("timed out");
}

/// A broadcast published before the grant arrives is checked at admission too.
async fn a_broadcast_published_before_the_grant_is_checked(version: &'static str) {
	within(async {
		let publisher = produce_origin(2);
		let early = publisher.create_broadcast("foo/bar").unwrap();
		early.announce(Default::default()).unwrap();

		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(publisher.clone()),
			server_subscribe: Some(produce_origin(1).scope("", &patterns(&["baz"])).unwrap()),
			..Default::default()
		})
		.await;

		assert!(matches!(
			pair.server.closed().await,
			Error::Session(SessionError::Unauthorized)
		));
		assert_eq!(
			pair.client_transport.close_reason().map(|(_, reason)| reason),
			Some("unauthorized: foo/bar".to_string())
		);
	})
	.await
	.expect("timed out");
}

/// Enforcement waits only for the tokens the session presented itself: a peer that
/// never answers an app-added token cannot suspend it.
async fn an_unanswered_token_does_not_suspend_the_check(version: &'static str) {
	within(async {
		let publisher = produce_origin(2);
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(publisher.clone()),
			server_subscribe: Some(produce_origin(1)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let _issued = serve(pair.requests.take().unwrap(), |token| {
			token.is_empty().then(|| grant(&["baz"], &[]))
		});

		let auth = pair.client.auth();
		let pending = tokio::spawn(async move { auth.add("never answered").await.map(|_| ()) });
		assert_eq!(granted(&pair.client).await, grant(&["baz"], &[]));

		let bad = publisher.create_broadcast("foo/bar").unwrap();
		bad.announce(Default::default()).unwrap();
		assert!(matches!(
			pair.server.closed().await,
			Error::Session(SessionError::Unauthorized)
		));
		// The session's close fails the token still waiting on its answer.
		assert!(pending.await.unwrap().is_err());
	})
	.await
	.expect("timed out");
}

/// Two tokens union; closing one shrinks the union and withdraws only what it alone
/// covered, without disconnecting.
async fn tokens_union_and_withdrawing_one_shrinks_it(version: &'static str) {
	within(async {
		let publisher = produce_origin(2);
		let relay = produce_origin(1);
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(publisher.clone()),
			server_subscribe: Some(relay.clone()),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut issued = serve(pair.requests.take().unwrap(), |token| match token {
			b"" => Some(grant(&["a"], &[])),
			b"t1" => Some(grant(&["b"], &[])),
			_ => None,
		});
		let (_, _setup) = issued.recv().await.unwrap();

		let t1 = pair.client.auth().add("t1").await.expect("t1 granted");
		let (_, t1_issued) = issued.recv().await.unwrap();
		assert_eq!(t1.grant().peek(), Some(grant(&["b"], &[])));
		assert_eq!(granted(&pair.client).await, grant(&["a", "b"], &[]));

		let a = publisher.create_broadcast("a/x").unwrap();
		a.announce(Default::default()).unwrap();
		let b = publisher.create_broadcast("b/y").unwrap();
		b.announce(Default::default()).unwrap();
		let served = relay.consume();
		wait_announced(&served, "a/x", true).await;
		wait_announced(&served, "b/y", true).await;

		drop(t1);
		// The acceptor learns the token is gone, and the presenter withdraws b/y.
		assert!(matches!(t1_issued.closed().await, Error::Cancel));
		wait_for(pair.client.auth().grant(), |g| g == &Some(grant(&["a"], &[]))).await;
		wait_announced(&served, "b/y", false).await;
		wait_announced(&served, "a/x", true).await;
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// An update replaces one token's grant and leaves the other alone.
async fn an_update_replaces_one_tokens_grant(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut issued = serve(pair.requests.take().unwrap(), |token| match token {
			b"" => Some(grant(&["a"], &[])),
			b"t1" => Some(grant(&["b"], &[])),
			_ => None,
		});
		let (_, _setup) = issued.recv().await.unwrap();
		let t1 = pair.client.auth().add("t1").await.unwrap();
		let (_, t1_issued) = issued.recv().await.unwrap();

		t1_issued.update(grant(&["c"], &["c"]));
		wait_for(t1.grant(), |g| g == &Some(grant(&["c"], &["c"]))).await;
		assert_eq!(granted(&pair.client).await, grant(&["a", "c"], &["c"]));
	})
	.await
	.expect("timed out");
}

/// A revocation withdraws what the grant covered, even though the broadcast stays in
/// the shared origin, and an empty union can be authorized again.
async fn a_revoked_grant_withdraws_and_can_be_restored(version: &'static str) {
	within(async {
		let publisher = produce_origin(2);
		let relay = produce_origin(1);
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(publisher.clone()),
			server_subscribe: Some(relay.clone()),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut issued = serve(pair.requests.take().unwrap(), |token| match token {
			b"" | b"again" => Some(grant(&["a"], &[])),
			_ => None,
		});
		let (_, setup) = issued.recv().await.unwrap();

		let a = publisher.create_broadcast("a/x").unwrap();
		a.announce(Default::default()).unwrap();
		let served = relay.consume();
		wait_announced(&served, "a/x", true).await;

		setup.revoke(SessionError::Unauthorized, "expired");
		wait_for(pair.client.auth().grant(), |g| g == &Some(Grant::default())).await;
		wait_announced(&served, "a/x", false).await;
		// Still published locally, and the session survives the empty union.
		assert!(publisher.consume().routed("a/x").await.is_some());
		assert_eq!(pair.client_transport.close_reason(), None);

		let _again = pair.client.auth().add("again").await.expect("re-authorized");
		wait_announced(&served, "a/x", true).await;
	})
	.await
	.expect("timed out");
}

/// A refused token surfaces the acceptor's code.
async fn a_refused_token_reports_the_code(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut requests = pair.requests.take().unwrap();
		tokio::spawn(async move {
			while let Some(request) = requests.next().await {
				match request.token().is_empty() {
					true => {
						std::mem::forget(request.accept(Grant::default()));
					}
					false => request.reject(SessionError::Unauthorized, "bad signature"),
				}
			}
		});

		let err = pair.client.auth().add("forged").await.err().expect("refused");
		assert!(matches!(err, Error::Session(SessionError::Unauthorized)), "{err:?}");
	})
	.await
	.expect("timed out");
}

/// Refusing the setup token grants nothing: the union becomes empty rather than
/// staying unknown, which the gates would read as unrestricted.
async fn a_refused_setup_token_grants_nothing(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut requests = pair.requests.take().unwrap();
		tokio::spawn(async move {
			while let Some(request) = requests.next().await {
				request.reject(SessionError::Unauthorized, "bad credential");
			}
		});

		assert_eq!(granted(&pair.client).await, Grant::default());
	})
	.await
	.expect("timed out");
}

/// Dropping the requests refuses tokens already queued, not only later ones.
async fn dropping_the_requests_refuses_queued_tokens(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut requests = pair.requests.take().unwrap();
		// Wait for the setup token to be queued, then hand it back to the queue's owner.
		let first = requests.next().await.expect("setup token");
		drop(first.accept(Grant::default()));
		let auth = pair.client.auth();
		let pending = tokio::spawn(async move { auth.add("queued").await.map(drop) });
		// Give the second token time to reach the queue before dropping it.
		tokio::time::sleep(Duration::from_millis(50)).await;
		drop(requests);

		let err = pending.await.unwrap().expect_err("refused");
		assert!(matches!(err, Error::Session(SessionError::Unauthorized)), "{err:?}");
	})
	.await
	.expect("timed out");
}

/// A closed session holds no grant: every token ended with it.
async fn a_closed_session_holds_no_grant(version: &'static str) {
	within(async {
		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			..Default::default()
		})
		.await;
		assert_ne!(granted(&pair.client).await, Grant::default());
		let token = pair.client.auth().grant();

		pair.server.abort(Error::Cancel);
		wait_for(token, |g| g == &Some(Grant::default())).await;
	})
	.await
	.expect("timed out");
}

/// A grant this side issued settles once its session ends, even when the task serving
/// its stream is dropped rather than run to completion.
async fn an_issued_grant_ends_with_its_session(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut requests = pair.requests.take().unwrap();
		let issued = requests.next().await.expect("setup token").accept(Grant::default());
		granted(&pair.client).await;

		pair.server_driver.abort();
		let err = issued.closed().await;
		assert!(matches!(err, Error::Cancel), "{err:?}");
	})
	.await
	.expect("timed out");
}

/// A peer that takes no tokens in band says so (lite resets the stream, moq-transport
/// answers NOT_SUPPORTED), which reads as unsupported rather than a refusal: the same as
/// a peer that predates AUTH.
async fn a_reset_auth_stream_reports_unsupported(version: &'static str) {
	within(async {
		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			..Default::default()
		})
		.await;
		granted(&pair.client).await;

		let err = pair.client.auth().add("token").await.err().expect("no acceptor");
		assert!(matches!(err, Error::Unsupported), "{err:?}");
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// Versions without AUTH never open the stream: no grant, and no way to add a token.
async fn older_versions_have_no_grant(version: &'static str) {
	within(async {
		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			..Default::default()
		})
		.await;
		assert_eq!(pair.client.auth().grant().peek(), None);
		assert!(matches!(pair.client.auth().add("token").await, Err(Error::Unsupported)));
	})
	.await
	.expect("timed out");
}

/// Losing a grant cancels the subscriptions it covered, in both directions, and
/// leaves the session up.
async fn a_revoked_grant_cancels_its_subscriptions(version: &'static str) {
	within(async {
		let ts = |ms| moq_net::Timestamp::from_millis(ms).unwrap();
		let prefs = || moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10));

		// The server publishes room/x to the client; the client publishes up/y to the server.
		let server_origin = produce_origin(1);
		let down = server_origin.create_broadcast("room/x").unwrap();
		let down_track = down.create_track("video", None).unwrap();
		down.announce(Default::default()).unwrap();

		let client_origin = produce_origin(2);
		let up = client_origin.create_broadcast("up/y").unwrap();
		let up_track = up.create_track("video", None).unwrap();
		up.announce(Default::default()).unwrap();

		let received = produce_origin(3);
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(client_origin.clone()),
			client_subscribe: Some(received.clone()),
			server_publish: Some(server_origin.clone()),
			server_subscribe: Some(server_origin.clone()),
			server_requests: true,
			client_request_token: None,
			..Default::default()
		})
		.await;
		let mut issued = serve(pair.requests.take().unwrap(), |token| {
			token.is_empty().then(|| grant(&["up"], &["room"]))
		});
		let (_, setup) = issued.recv().await.unwrap();

		let mut group = down_track.append_group().unwrap();
		group.write_frame(ts(0), b"down".as_ref()).unwrap();
		let mut group = up_track.append_group().unwrap();
		group.write_frame(ts(0), b"up".as_ref()).unwrap();

		let remote = received.consume().routed_broadcast("room/x").await.unwrap();
		let mut down_sub = remote.track("video").unwrap().subscribe(prefs()).await.unwrap();
		down_sub.recv_group().await.unwrap().unwrap();

		let remote = server_origin.consume().routed_broadcast("up/y").await.unwrap();
		let mut up_sub = remote.track("video").unwrap().subscribe(prefs()).await.unwrap();
		up_sub.recv_group().await.unwrap().unwrap();

		setup.revoke(SessionError::Unauthorized, "expired");
		let err = down_sub
			.recv_group()
			.await
			.err()
			.expect("subscription outlived its grant");
		assert!(matches!(err, Error::Unauthorized), "{err:?}");
		// The served side ends too: the client stops serving what it may no longer publish.
		let err = loop {
			match up_sub.recv_group().await {
				Ok(Some(_)) => continue,
				Ok(None) => panic!("served subscription finished instead of ending"),
				Err(err) => break err,
			}
		};
		// The relay's own reader learns the peer's code across the origin's splice, not a
		// generic drop. moq-transport reports it in PUBLISH_DONE instead.
		if version == LITE_06 {
			assert!(matches!(err, Error::Stream(StreamError::Unauthorized)), "{err:?}");
		}
		assert_eq!(pair.client_transport.close_reason(), None);

		// Neither stream claims the session closed. moq-lite resets both with UNAUTHORIZED;
		// moq-transport has no such stream code and reports it on the request instead.
		let resets = pair.client_transport.resets();
		let closed = StreamError::Session(SessionError::Unauthorized).to_code();
		assert!(!resets.contains(&closed), "{resets:x?}");
		if version == LITE_06 {
			let unauthorized = StreamError::Unauthorized.to_code();
			let count = resets.iter().filter(|&&code| code == unauthorized).count();
			assert_eq!(count, 2, "both subscriptions reset with UNAUTHORIZED: {resets:x?}");
		}
	})
	.await
	.expect("timed out");
}

/// A grant moq-transport cannot carry as prefixes is never widened: the token is refused
/// as unsupported, promptly, and the union stays unknown rather than empty.
async fn an_unrepresentable_grant_is_unsupported(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut issued = serve(pair.requests.take().unwrap(), |token| match token {
			b"" => Some(grant(&["a"], &[])),
			b"exact" => Some(Grant {
				publish: Patterns::from(Pattern::try_from("room/alice").unwrap()),
				subscribe: Patterns::new(),
				expires: None,
			}),
			b"mixed" => Some(Grant {
				publish: ["room/**", "lobby"]
					.into_iter()
					.map(|p| Pattern::try_from(p).unwrap())
					.collect(),
				subscribe: Patterns::new(),
				expires: None,
			}),
			b"wildcard" => Some(Grant {
				publish: Patterns::from(Pattern::try_from("room/*/cam").unwrap()),
				subscribe: Patterns::new(),
				expires: None,
			}),
			_ => None,
		});
		let (_, _setup) = issued.recv().await.unwrap();
		assert_eq!(granted(&pair.client).await, grant(&["a"], &[]));

		for token in ["exact", "mixed", "wildcard"] {
			let err = pair.client.auth().add(token).await.err().expect("not representable");
			assert!(matches!(err, Error::Unsupported), "{token}: {err:?}");
		}
		// The other token is untouched, and so is the session.
		assert_eq!(pair.client.auth().grant().peek(), Some(grant(&["a"], &[])));
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// An update moq-transport cannot carry revokes that token's earlier grant, and only
/// that token's: the rest of the union and the session stay.
async fn an_unrepresentable_update_revokes_only_its_token(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut issued = serve(pair.requests.take().unwrap(), |token| match token {
			b"" => Some(grant(&["a"], &[])),
			b"t1" => Some(grant(&["b"], &[])),
			_ => None,
		});
		let (_, _setup) = issued.recv().await.unwrap();
		let t1 = pair.client.auth().add("t1").await.unwrap();
		let (_, t1_issued) = issued.recv().await.unwrap();
		assert_eq!(granted(&pair.client).await, grant(&["a", "b"], &[]));

		t1_issued.update(Grant {
			publish: Patterns::from(Pattern::try_from("b/exact").unwrap()),
			subscribe: Patterns::new(),
			expires: None,
		});
		t1.closed().await;
		assert_eq!(t1.grant().peek(), None);
		wait_for(pair.client.auth().grant(), |g| g == &Some(grant(&["a"], &[]))).await;
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// A grant that fits no single AUTH_OK is withheld, never trimmed: the presenter is told
/// it is unsupported, nothing of the message reaches the wire, and the session and the
/// token that did fit are untouched.
async fn a_grant_too_large_for_one_message_is_unsupported(version: &'static str) {
	// Past the u16 message size. The moq-lite ceiling is 64 MiB, too slow to build here;
	// `lite::auth` tests it on the encoder.
	let (count, len) = (20, 4_000);
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let huge: Patterns = (0..count)
			.map(|i| Pattern::subtree(&format!("{i}{}", "x".repeat(len))).unwrap())
			.collect();
		let mut issued = serve(pair.requests.take().unwrap(), move |token| match token {
			b"" => Some(grant(&["a"], &[])),
			b"huge" => Some(Grant {
				publish: huge.clone(),
				subscribe: Patterns::new(),
				expires: None,
			}),
			_ => None,
		});
		let (_, _setup) = issued.recv().await.unwrap();
		assert_eq!(granted(&pair.client).await, grant(&["a"], &[]));

		let err = pair.client.auth().add("huge").await.err().expect("too large");
		assert!(matches!(err, Error::Unsupported), "{err:?}");
		// No AUTH_OK for it reached the wire: the union is still just the setup grant.
		assert_eq!(pair.client.auth().grant().peek(), Some(grant(&["a"], &[])));
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

fn pattern_set(texts: &[&str]) -> Patterns {
	texts.iter().map(|text| Pattern::try_from(*text).unwrap()).collect()
}

/// Literal, wildcard, and mixed grants reach the presenter exactly as issued, never
/// widened to a covering prefix.
async fn pattern_grants_arrive_exactly(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let table = |token: &[u8]| -> Option<Grant> {
			let (publish, subscribe) = match token {
				b"" => (pattern_set(&["a/**"]), pattern_set(&[])),
				b"exact" => (pattern_set(&["room/alice"]), pattern_set(&[])),
				b"wildcard" => (pattern_set(&["room/*/cam"]), pattern_set(&["**/demo.hang"])),
				b"mixed" => (pattern_set(&["room/**", "lobby", "cam-*.hang"]), pattern_set(&[])),
				b"root" => (pattern_set(&[""]), pattern_set(&["**"])),
				_ => return None,
			};
			Some(Grant {
				publish,
				subscribe,
				expires: None,
			})
		};
		let mut issued = serve(pair.requests.take().unwrap(), table);
		let (_, _setup) = issued.recv().await.unwrap();
		assert_eq!(granted(&pair.client).await, table(b"").unwrap());

		let mut held = Vec::new();
		for token in ["exact", "wildcard", "mixed", "root"] {
			let added = pair.client.auth().add(token).await.expect(token);
			assert_eq!(added.grant().peek(), table(token.as_bytes()), "{token}");
			held.push(added);
		}
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// A wildcard grant admits what it matches, including a leading `**` matching zero
/// segments, and a publish outside it still aborts naming the path.
async fn a_wildcard_grant_is_enforced(version: &'static str) {
	within(async {
		let publisher = produce_origin(2);
		let relay = produce_origin(1);
		let scope = pattern_set(&["room/*/cam", "**/b.hang"]);
		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(publisher.clone()),
			server_subscribe: Some(relay.scope("", &scope).unwrap()),
			..Default::default()
		})
		.await;
		assert_eq!(granted(&pair.client).await.publish, scope);

		let mut held = Vec::new();
		for path in ["room/alice/cam", "b.hang", "deep/x/b.hang"] {
			let broadcast = publisher.create_broadcast(path).unwrap();
			broadcast.announce(Default::default()).unwrap();
			wait_announced(&relay.consume(), path, true).await;
			held.push(broadcast);
		}
		assert_eq!(pair.client_transport.close_reason(), None);

		let bad = publisher.create_broadcast("room/alice/mic").unwrap();
		bad.announce(Default::default()).unwrap();
		assert!(matches!(
			pair.server.closed().await,
			Error::Session(SessionError::Unauthorized)
		));
		assert_eq!(
			pair.client_transport.close_reason().map(|(_, reason)| reason),
			Some("unauthorized: room/alice/mic".to_string())
		);
	})
	.await
	.expect("timed out");
}

/// The session closes before the peer ever hears of a broadcast outside the grant,
/// even one published before the grant arrived: nothing is advertised until the
/// setup token is answered.
async fn nothing_outside_the_grant_reaches_the_peer(version: &'static str) {
	within(async {
		let publisher = produce_origin(2);
		let early = publisher.create_broadcast("foo/bar").unwrap();
		early.announce(Default::default()).unwrap();

		// The relay accepts anything; only the grant it tells the client is narrow.
		let relay = produce_origin(1);
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(publisher.clone()),
			server_subscribe: Some(relay.clone()),
			server_requests: true,
			..Default::default()
		})
		.await;

		// Hold the answer, so the peer's discovery request is in long before the grant.
		let mut requests = pair.requests.take().unwrap();
		let setup = requests.next().await.expect("setup token");
		let leaked = tokio::time::timeout(
			Duration::from_millis(100),
			wait_announced(&relay.consume(), "foo/bar", true),
		)
		.await;
		assert!(leaked.is_err(), "advertised before the grant was known");

		let _issued = setup.accept(grant(&["baz"], &[]));
		assert!(matches!(
			pair.server.closed().await,
			Error::Session(SessionError::Unauthorized)
		));
	})
	.await
	.expect("timed out");
}

/// Run each limit case on every version family: the session enforces its limit itself,
/// so a peer without AUTH (or one that ignores it) cannot keep what it lost.
macro_rules! limit_cases {
	($($case:ident),* $(,)?) => {
		mod limit_lite_05 {
			$(#[tokio::test] async fn $case() { super::$case("moq-lite-05").await })*
		}
		mod limit_lite_06 {
			$(#[tokio::test] async fn $case() { super::$case(super::LITE_06).await })*
		}
		mod limit_moqt_16 {
			$(#[tokio::test] async fn $case() { super::$case("moq-transport-16").await })*
		}
		mod limit_moqt_17 {
			$(#[tokio::test] async fn $case() { super::$case(super::MOQT_17).await })*
		}
	};
}

limit_cases!(
	a_narrowing_deafens_one_path,
	a_narrowing_aborts_what_the_peer_published,
	a_widening_brings_back_a_deafened_path,
	a_widening_brings_back_what_the_peer_published,
);

#[tokio::test]
async fn lite_05_narrowing_resets_a_fetch_in_flight() {
	a_narrowing_resets_a_fetch_in_flight("moq-lite-05").await
}

#[tokio::test]
async fn lite_06_narrowing_resets_a_fetch_in_flight() {
	a_narrowing_resets_a_fetch_in_flight(LITE_06).await
}

/// Whether `version` exchanges AUTH, so the peer also hears of a narrowing.
fn speaks_auth(version: &str) -> bool {
	matches!(version, LITE_06 | MOQT_17 | MOQT_22)
}

/// A revocation as the reader sees it: the local gate's own error, or the peer's
/// UNAUTHORIZED reset relayed across the splice.
fn unauthorized(err: &Error) -> bool {
	matches!(err, Error::Unauthorized | Error::Stream(StreamError::Unauthorized))
}

/// Read groups until the subscription ends, returning how it ended.
async fn ended(sub: &mut moq_net::track::Subscriber) -> Error {
	loop {
		match sub.recv_group().await {
			Ok(Some(_)) => continue,
			Ok(None) => panic!("subscription finished instead of ending"),
			Err(err) => return err,
		}
	}
}

/// The deafen case: narrowing a live session away from one audio path resets that
/// subscription and retracts its announcement, while a sibling under the same prefix
/// keeps flowing and the session stays up.
async fn a_narrowing_deafens_one_path(version: &'static str) {
	within(async {
		let ts = |ms| moq_net::Timestamp::from_millis(ms).unwrap();
		let prefs = || moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10));

		let relay = produce_origin(1);
		let audio = relay.create_broadcast("room/alice/audio").unwrap();
		let audio_track = audio.create_track("opus", None).unwrap();
		audio.announce(Default::default()).unwrap();
		let video = relay.create_broadcast("room/alice/video").unwrap();
		let video_track = video.create_track("h264", None).unwrap();
		video.announce(Default::default()).unwrap();

		let received = produce_origin(3);
		let pair = connect(Options {
			version: Some(version),
			client_subscribe: Some(received.clone()),
			server_publish: Some(relay.scope("", &patterns(&["room"])).unwrap()),
			..Default::default()
		})
		.await;

		let mut group = audio_track.append_group().unwrap();
		group.write_frame(ts(0), b"a".as_ref()).unwrap();
		let mut group = video_track.append_group().unwrap();
		group.write_frame(ts(0), b"v".as_ref()).unwrap();

		let remote = received.consume().routed_broadcast("room/alice/audio").await.unwrap();
		let mut audio_sub = remote.track("opus").unwrap().subscribe(prefs()).await.unwrap();
		audio_sub.recv_group().await.unwrap().unwrap();
		let remote = received.consume().routed_broadcast("room/alice/video").await.unwrap();
		let mut video_sub = remote.track("h264").unwrap().subscribe(prefs()).await.unwrap();
		video_sub.recv_group().await.unwrap().unwrap();

		pair.server.auth().authorize(&grant(&[], &["room/alice/video"]));

		let err = ended(&mut audio_sub).await;
		assert!(unauthorized(&err), "{err:?}");
		wait_announced(&received.consume(), "room/alice/audio", false).await;

		// The sibling keeps flowing.
		let mut group = video_track.append_group().unwrap();
		group.write_frame(ts(1), b"v".as_ref()).unwrap();
		let group = video_sub.recv_group().await.unwrap().expect("video still flows");
		assert_eq!(group.sequence, 1);

		// The relay enforced it, not the client: moq-lite resets the subscription with
		// UNAUTHORIZED, and neither side closed the session.
		if version.starts_with("moq-lite") {
			let resets = pair.server_transport.resets();
			assert!(resets.contains(&StreamError::Unauthorized.to_code()), "{resets:x?}");
		}
		assert_eq!(pair.client_transport.close_reason(), None);

		// A peer that speaks AUTH is told what it may still subscribe to.
		if speaks_auth(version) {
			let narrowed = wait_for(pair.client.auth().grant(), |grant| {
				grant
					.as_ref()
					.is_some_and(|grant| grant.subscribe == patterns(&["room/alice/video"]))
			})
			.await;
			assert_eq!(narrowed, Some(grant(&[], &["room/alice/video"])));
		}
	})
	.await
	.expect("timed out");
}

/// Narrowing what the peer may publish aborts the broadcasts it published outside,
/// so the relay's own readers see `Unauthorized`, and retracts their routes, while
/// what it may still publish keeps flowing.
async fn a_narrowing_aborts_what_the_peer_published(version: &'static str) {
	within(async {
		let ts = |ms| moq_net::Timestamp::from_millis(ms).unwrap();
		let prefs = || moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10));

		let client_origin = produce_origin(2);
		let mic = client_origin.create_broadcast("room/bob/mic").unwrap();
		let mic_track = mic.create_track("opus", None).unwrap();
		mic.announce(Default::default()).unwrap();
		let cam = client_origin.create_broadcast("room/bob/cam").unwrap();
		let cam_track = cam.create_track("h264", None).unwrap();
		cam.announce(Default::default()).unwrap();

		let relay = produce_origin(1);
		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(client_origin.clone()),
			server_subscribe: Some(relay.clone()),
			..Default::default()
		})
		.await;

		let mut group = mic_track.append_group().unwrap();
		group.write_frame(ts(0), b"m".as_ref()).unwrap();
		let mut group = cam_track.append_group().unwrap();
		group.write_frame(ts(0), b"c".as_ref()).unwrap();

		let remote = relay.consume().routed_broadcast("room/bob/mic").await.unwrap();
		let mut mic_sub = remote.track("opus").unwrap().subscribe(prefs()).await.unwrap();
		mic_sub.recv_group().await.unwrap().unwrap();
		let remote = relay.consume().routed_broadcast("room/bob/cam").await.unwrap();
		let mut cam_sub = remote.track("h264").unwrap().subscribe(prefs()).await.unwrap();
		cam_sub.recv_group().await.unwrap().unwrap();

		pair.server.auth().authorize(&grant(&["room/bob/cam"], &[]));

		let err = ended(&mut mic_sub).await;
		assert!(unauthorized(&err), "{err:?}");
		wait_announced(&relay.consume(), "room/bob/mic", false).await;

		let mut group = cam_track.append_group().unwrap();
		group.write_frame(ts(1), b"c".as_ref()).unwrap();
		let group = cam_sub.recv_group().await.unwrap().expect("cam still flows");
		assert_eq!(group.sequence, 1);

		// A narrowing is not a publication outside the grant: the client stays up.
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// A fetch still streaming its group when the grant narrows away from it is reset with
/// UNAUTHORIZED by the publisher, not left to finish.
async fn a_narrowing_resets_a_fetch_in_flight(version: &'static str) {
	within(async {
		let ts = |ms| moq_net::Timestamp::from_millis(ms).unwrap();

		let relay = produce_origin(1);
		let broadcast = relay.create_broadcast("room/x").unwrap();
		let track = broadcast.create_track("video", None).unwrap();
		broadcast.announce(Default::default()).unwrap();
		// The group stays open, so the fetch is still in flight when the grant narrows.
		let mut group = track.append_group().unwrap();
		group.write_frame(ts(0), b"first".as_ref()).unwrap();

		let received = produce_origin(3);
		let pair = connect(Options {
			version: Some(version),
			client_subscribe: Some(received.clone()),
			server_publish: Some(relay.clone()),
			..Default::default()
		})
		.await;

		let remote = received.consume().routed_broadcast("room/x").await.unwrap();
		let mut fetched = remote.track("video").unwrap().fetch_group(0, None).await.unwrap();
		let frame = fetched.read_frame().await.unwrap().expect("first frame");
		assert_eq!(frame.payload.as_ref(), b"first");

		pair.server.auth().authorize(&grant(&[], &["room/y"]));

		let err = loop {
			match fetched.read_frame().await {
				Ok(Some(_)) => continue,
				Ok(None) => panic!("fetch finished instead of ending"),
				Err(err) => break err,
			}
		};
		assert!(unauthorized(&err), "{err:?}");
		let resets = pair.server_transport.resets();
		assert!(resets.contains(&StreamError::Unauthorized.to_code()), "{resets:x?}");
		drop(group);
	})
	.await
	.expect("timed out");
}

/// Read groups until one at `sequence` or later arrives.
async fn recv_through(sub: &mut moq_net::track::Subscriber, sequence: u64) {
	loop {
		let group = sub.recv_group().await.unwrap().expect("track ended");
		if group.sequence >= sequence {
			return;
		}
	}
}

/// Widening the limit after a narrowing brings the deafened path back: it is announced
/// again and a new subscription to it flows.
async fn a_widening_brings_back_a_deafened_path(version: &'static str) {
	within(async {
		let ts = |ms| moq_net::Timestamp::from_millis(ms).unwrap();
		let prefs = || moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10));

		let relay = produce_origin(1);
		let audio = relay.create_broadcast("room/alice/audio").unwrap();
		let audio_track = audio.create_track("opus", None).unwrap();
		audio.announce(Default::default()).unwrap();

		let received = produce_origin(3);
		let pair = connect(Options {
			version: Some(version),
			client_subscribe: Some(received.clone()),
			server_publish: Some(relay.scope("", &patterns(&["room"])).unwrap()),
			..Default::default()
		})
		.await;

		let mut group = audio_track.append_group().unwrap();
		group.write_frame(ts(0), b"a".as_ref()).unwrap();
		let remote = received.consume().routed_broadcast("room/alice/audio").await.unwrap();
		let mut sub = remote.track("opus").unwrap().subscribe(prefs()).await.unwrap();
		sub.recv_group().await.unwrap().unwrap();

		pair.server.auth().authorize(&grant(&[], &["room/alice/video"]));
		let err = ended(&mut sub).await;
		assert!(unauthorized(&err), "{err:?}");
		wait_announced(&received.consume(), "room/alice/audio", false).await;
		drop((sub, remote));

		pair.server.auth().authorize(&grant(&[], &["room"]));
		wait_announced(&received.consume(), "room/alice/audio", true).await;
		let remote = received.consume().routed_broadcast("room/alice/audio").await.unwrap();
		let mut sub = remote.track("opus").unwrap().subscribe(prefs()).await.unwrap();
		let mut group = audio_track.append_group().unwrap();
		group.write_frame(ts(1), b"a".as_ref()).unwrap();
		recv_through(&mut sub, 1).await;

		// A peer that speaks AUTH is told it may subscribe again.
		if speaks_auth(version) {
			let widened = wait_for(pair.client.auth().grant(), |grant| {
				grant
					.as_ref()
					.is_some_and(|grant| grant.subscribe == patterns(&["room"]))
			})
			.await;
			assert_eq!(widened, Some(grant(&[], &["room"])));
		}
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// Widening the limit after a narrowing brings back what the peer published: its route
/// is in the origin again and the relay's own readers can subscribe to it.
async fn a_widening_brings_back_what_the_peer_published(version: &'static str) {
	within(async {
		let ts = |ms| moq_net::Timestamp::from_millis(ms).unwrap();
		let prefs = || moq_net::track::Subscription::default().with_max_age(Duration::from_secs(10));

		let client_origin = produce_origin(2);
		let mic = client_origin.create_broadcast("room/bob/mic").unwrap();
		let mic_track = mic.create_track("opus", None).unwrap();
		mic.announce(Default::default()).unwrap();

		let relay = produce_origin(1);
		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(client_origin.clone()),
			server_subscribe: Some(relay.clone()),
			..Default::default()
		})
		.await;
		wait_announced(&relay.consume(), "room/bob/mic", true).await;

		pair.server.auth().authorize(&grant(&["room/bob/cam"], &[]));
		wait_announced(&relay.consume(), "room/bob/mic", false).await;

		pair.server.auth().authorize(&grant(&["room"], &[]));
		wait_announced(&relay.consume(), "room/bob/mic", true).await;
		let remote = relay.consume().routed_broadcast("room/bob/mic").await.unwrap();
		let mut sub = remote.track("opus").unwrap().subscribe(prefs()).await.unwrap();
		let mut group = mic_track.append_group().unwrap();
		group.write_frame(ts(0), b"m".as_ref()).unwrap();
		recv_through(&mut sub, 0).await;

		// Neither the narrowing nor the widening is a publication outside the grant.
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}
