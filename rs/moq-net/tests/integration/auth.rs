#![cfg(feature = "async")]

//! In-band AUTH over the in-memory mock transport: both sides learn their grant,
//! tokens union, and a publication outside the grant fails loud. Every case runs on
//! moq-lite-07-wip and on moq-transport with the MoQ Auth extension.

use crate::support;

use std::time::Duration;

use futures::StreamExt;
use moq_net::{
	Client, Error, Hop, Pattern, Patterns, Server, Session, SessionError, StreamError, Version,
	auth::{self, Grant},
	origin,
};
use support::harness::{now, spawn};
use support::mock::{MockSession, create_mock_session_pair};

/// A millisecond track: the frames written here carry timestamps.
fn timed() -> moq_net::track::Info {
	moq_net::track::Info::default().with_timescale(moq_net::Timescale::MILLI)
}

/// Maximum time any single test may run before being treated as a deadlock.
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The first lite version with the Auth Stream, still work-in-progress.
const LITE_07: &str = "moq-lite-07-wip";
/// The newest published lite version, which has no Auth Stream.
const LITE_06: &str = "moq-lite-06";
/// The lite Auth Stream's type byte.
const AUTH_STREAM: u8 = 0x7;
/// The oldest draft we speak, which negotiates MoQ Auth, and the newest.
const MOQT_18: &str = "moq-transport-18";
const MOQT_22: &str = "moq-transport-22";

/// Run each case on every version that exchanges AUTH.
macro_rules! cases {
	($($case:ident),* $(,)?) => {
		mod lite_07 {
			$(#[moq_clock::test] async fn $case() { super::$case(super::LITE_07).await })*
		}
		mod moqt_18 {
			$(#[moq_clock::test] async fn $case() { super::$case(super::MOQT_18).await })*
		}
		mod moqt_22 {
			$(#[moq_clock::test] async fn $case() { super::$case(super::MOQT_22).await })*
		}
	};
}

cases!(
	both_sides_learn_their_grant_from_scoped_origins,
	a_publish_only_session_grants_no_subscribe,
	an_out_of_scope_announce_aborts_with_the_path,
	a_broadcast_published_before_the_grant_is_checked,
	an_unanswered_token_holds_the_check,
	a_broadcast_withdrawn_while_its_token_waits_is_not_refused,
	tokens_union_and_withdrawing_one_shrinks_it,
	an_update_replaces_one_tokens_grant,
	a_revoked_grant_withdraws_and_can_be_restored,
	a_refused_token_reports_the_code,
	a_refused_setup_token_grants_nothing,
	the_setup_token_is_the_connection_credential,
	dropping_the_requests_refuses_queued_tokens,
	a_closed_session_holds_no_grant,
	an_issued_grant_ends_with_its_session,
	a_reset_auth_stream_reports_unsupported,
	a_revoked_grant_cancels_its_subscriptions,
	a_live_subscription_survives_union_renewal,
	a_grant_update_too_large_for_one_message_is_unsupported,
	nothing_outside_the_grant_reaches_the_peer,
	a_shared_handle_presents_its_token_on_every_session,
	a_handle_backs_one_active_session,
	a_token_queued_before_our_goaway_rides_only_the_successor,
	a_broadcast_received_from_the_peer_is_not_ours_to_check,
	exact_and_subtree_grants_admit_their_broadcast,
	pattern_grants_arrive_exactly,
	a_wildcard_grant_is_enforced,
);

/// moq-transport's AUTH_ERROR carries no code that may pass, so only lite retries.
#[moq_clock::test]
async fn lite_07_presents_a_transiently_refused_token_again() {
	a_transient_refusal_is_presented_again(LITE_07).await
}

/// moq-transport's u16 message size is the one a test can overflow quickly.
#[moq_clock::test]
async fn moqt_18_a_grant_too_large_for_one_message_is_unsupported() {
	a_grant_too_large_for_one_message_is_unsupported(MOQT_18).await
}

#[moq_clock::test]
async fn moqt_22_a_grant_too_large_for_one_message_is_unsupported() {
	a_grant_too_large_for_one_message_is_unsupported(MOQT_22).await
}

#[moq_clock::test]
async fn lite_06_has_no_grant() {
	older_versions_have_no_grant(LITE_06).await
}

/// The control for the lite versions without AUTH: on the same barrier, lite-07 has
/// opened an Auth Stream on each side, which is what makes its absence on them mean
/// something.
#[moq_clock::test]
async fn lite_07_opens_an_auth_stream_per_side() {
	within(async {
		let (pair, _broadcast) = connect_announced(LITE_07).await;
		for transport in [&pair.client_transport, &pair.server_transport] {
			assert!(
				transport.bidi_types().contains(&AUTH_STREAM),
				"{:x?}",
				transport.bidi_types()
			);
		}
	})
	.await
	.expect("timed out");
}

#[moq_clock::test]
async fn lite_05_has_no_grant() {
	older_versions_have_no_grant("moq-lite-05").await
}

#[moq_clock::test]
async fn moqt_22_route_auth_renews_its_announce() {
	a_route_auth_renews_its_announce(MOQT_22).await
}

#[moq_clock::test]
async fn moqt_22_route_auth_survives_a_reconnect() {
	a_route_auth_survives_a_reconnect(MOQT_22).await
}

#[moq_clock::test]
async fn moqt_22_renewing_a_cancelled_announce_keeps_the_session() {
	within(async {
		let (mut pair, client_origin, _server_origin) = connect_route_auth(MOQT_22).await;
		let mut tokens = serve(pair.requests.take().unwrap(), |_| Some(Grant::all()));
		let (first, answer) = pair.client.auth().add("first").await.expect("kept for requests");
		assert_eq!(answer, auth::Answer::Requests);
		let broadcast = client_origin.create_broadcast("room/x").unwrap();
		broadcast.announce(origin::Route::default().with_auth(first)).unwrap();
		let (_, first) = tokens.next().await.expect("the announce's token");

		// Ending the grant cancels the announcement. The idle request reads that and the
		// retry offers it again under the token the route still holds, so the swap renews
		// the new request.
		drop(first);
		let (token, _again) = tokens.next().await.expect("offered again after the cancel");
		assert_eq!(token, b"first");
		moq_clock::sleep(Duration::from_secs(1)).await;
		let (second, _) = pair.client.auth().add("second").await.expect("kept for requests");
		broadcast.announce(origin::Route::default().with_auth(second)).unwrap();
		let (token, _second) = tokens.next().await.expect("announced again");
		assert_eq!(token, b"second");
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

#[moq_clock::test]
async fn moqt_22_a_cancelled_renewal_keeps_the_session() {
	within(async {
		let (mut pair, client_origin, _server_origin) = connect_route_auth(MOQT_22).await;
		// The renewal's token goes unanswered, so its update is still in flight when the
		// first grant ends and the server cancels the announcement.
		let mut tokens = serve(pair.requests.take().unwrap(), |token| {
			(token == b"first").then(Grant::all)
		});
		let (first, _) = pair.client.auth().add("first").await.expect("kept for requests");
		let broadcast = client_origin.create_broadcast("room/x").unwrap();
		broadcast.announce(origin::Route::default().with_auth(first)).unwrap();
		let (_, first) = tokens.next().await.expect("the announce's token");

		let (second, _) = pair.client.auth().add("second").await.expect("kept for requests");
		broadcast.announce(origin::Route::default().with_auth(second)).unwrap();
		moq_clock::sleep(Duration::from_secs(1)).await;
		drop(first);
		moq_clock::sleep(Duration::from_secs(1)).await;
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// An acceptor that accepts a renewal and drops the grant it replaces in one step keeps the
/// announcement: the old grant ending is not the request ending once its replacement is in.
/// The replacement then holds it, so an update that stops covering it ends it.
#[moq_clock::test]
async fn moqt_22_dropping_the_old_grant_with_its_renewal_keeps_the_announce() {
	within(async {
		let (mut pair, client_origin, server_origin) = connect_route_auth(MOQT_22).await;
		let mut requests = pair.requests.take().unwrap();
		let (first, _) = pair.client.auth().add("first").await.expect("kept for requests");
		let broadcast = client_origin.create_broadcast("room/x").unwrap();
		broadcast.announce(origin::Route::default().with_auth(first)).unwrap();
		let first = requests
			.next()
			.await
			.expect("the announce's token")
			.accept(Grant::all());
		wait_announced(&server_origin.consume(), "room/x", true).await;

		let (second, _) = pair.client.auth().add("second").await.expect("kept for requests");
		broadcast.announce(origin::Route::default().with_auth(second)).unwrap();
		let renewal = requests.next().await.expect("the renewal's token");
		assert_eq!(renewal.token().as_ref(), b"second");
		let second = renewal.accept(Grant::all());
		drop(first);

		moq_clock::sleep(Duration::from_secs(1)).await;
		assert!(
			futures::FutureExt::now_or_never(second.closed()).is_none(),
			"the request ended with the grant its renewal replaced"
		);

		second.update(Grant::default());
		second.closed().await;
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// A route auth the peer revokes on its AUTH stream is no longer carried, so its request is
/// made again without it, even while another token keeps the session's grant unchanged.
#[moq_clock::test]
async fn moqt_22_a_revoked_route_auth_remakes_its_announce() {
	within(async {
		let client_origin = produce_origin(2);
		let server_origin = produce_origin(1);
		let mut extensions = moq_net::setup::Extensions::default();
		extensions.solicit = false;
		let mut pair = connect(Options {
			version: Some(MOQT_22),
			client_publish: Some(client_origin.clone()),
			server_subscribe: Some(server_origin.clone()),
			server_requests: true,
			server_extensions: Some(extensions),
			..Default::default()
		})
		.await;
		let mut tokens = serve(pair.requests.take().unwrap(), |_| Some(Grant::all()));
		let (token, _setup) = tokens.next().await.expect("the setup token");
		assert!(token.is_empty());

		let (first, _) = pair.client.auth().add("first").await.expect("granted");
		let (token, presented) = tokens.next().await.expect("the AUTH stream's token");
		assert_eq!(token, b"first");
		let broadcast = client_origin.create_broadcast("room/x").unwrap();
		broadcast.announce(origin::Route::default().with_auth(first)).unwrap();
		let (token, carried) = tokens.next().await.expect("the announce's token");
		assert_eq!(token, b"first");
		wait_announced(&server_origin.consume(), "room/x", true).await;

		presented.revoke(SessionError::Unauthorized, "revoked");
		carried.closed().await;
		wait_announced(&server_origin.consume(), "room/x", true).await;
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// A pair where the client declines MoQ Auth, so every token the server sees rode a
/// request, and the server declines MoQ Solicit, so the client announces with
/// PUBLISH_NAMESPACE rather than answering a SUBSCRIBE_NAMESPACE.
async fn connect_route_auth(version: &'static str) -> (Pair, origin::Producer, origin::Producer) {
	let client_origin = produce_origin(2);
	let server_origin = produce_origin(1);
	let pair = connect_route_auth_on(version, &client_origin, &server_origin, None).await;
	(pair, client_origin, server_origin)
}

/// [`connect_route_auth`] between existing origins, optionally on a shared client handle.
async fn connect_route_auth_on(
	version: &'static str,
	client_origin: &origin::Producer,
	server_origin: &origin::Producer,
	client_auth: Option<auth::Handle>,
) -> Pair {
	let declined = |f: fn(&mut moq_net::setup::Extensions)| {
		let mut extensions = moq_net::setup::Extensions::default();
		f(&mut extensions);
		Some(extensions)
	};
	connect(Options {
		version: Some(version),
		client_publish: Some(client_origin.clone()),
		server_subscribe: Some(server_origin.clone()),
		server_requests: true,
		client_extensions: declined(|e| e.auth = false),
		server_extensions: declined(|e| e.solicit = false),
		client_auth,
		..Default::default()
	})
	.await
}

/// A route naming a shared handle's auth carries it on the next session too, with no
/// re-add or re-announce from the application.
async fn a_route_auth_survives_a_reconnect(version: &'static str) {
	within(async {
		let handle = auth::Handle::new();
		let client_origin = produce_origin(2);
		let server_origin = produce_origin(1);
		let mut first = connect_route_auth_on(version, &client_origin, &server_origin, Some(handle.clone())).await;
		let mut tokens = serve(first.requests.take().unwrap(), |_| Some(Grant::all()));
		let (token, _) = handle.add("first").await.expect("kept for requests");
		let broadcast = client_origin.create_broadcast("room/x").unwrap();
		broadcast.announce(origin::Route::default().with_auth(token)).unwrap();
		let (token, _first) = tokens.next().await.expect("the announce's token");
		assert_eq!(token, b"first");

		first.server.abort(Error::Cancel);
		first.client.closed().await;

		let mut second = connect_route_auth_on(version, &client_origin, &server_origin, Some(handle)).await;
		let mut tokens = serve(second.requests.take().unwrap(), |_| Some(Grant::all()));
		let (token, _second) = tokens.next().await.expect("the announce's token on the new session");
		assert_eq!(token, b"first");
		wait_announced(&server_origin.consume(), "room/x", true).await;
	})
	.await
	.expect("timed out");
}

/// A route's auth reaches the peer's acceptor on the announcement, and swapping it
/// renews the announcement with the new token, keeping it live.
async fn a_route_auth_renews_its_announce(version: &'static str) {
	within(async {
		let (mut pair, client_origin, server_origin) = connect_route_auth(version).await;
		let mut tokens = serve(pair.requests.take().unwrap(), |_| Some(Grant::all()));

		let (first, _) = pair.client.auth().add("first").await.expect("kept for requests");
		let broadcast = client_origin.create_broadcast("room/x").unwrap();
		broadcast.announce(origin::Route::default().with_auth(first)).unwrap();
		let (token, _first) = tokens.next().await.expect("the announce's token");
		assert_eq!(token, b"first");
		wait_announced(&server_origin.consume(), "room/x", true).await;

		let (second, _) = pair.client.auth().add("second").await.expect("kept for requests");
		broadcast.announce(origin::Route::default().with_auth(second)).unwrap();
		let (token, _second) = tokens.next().await.expect("the renewal's token");
		assert_eq!(token, b"second");
		wait_announced(&server_origin.consume(), "room/x", true).await;
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// Build an origin producer, spawning its driver on the ambient runtime.
fn produce_origin(hop: u64) -> origin::Producer {
	let (producer, driver) = origin::Producer::new(origin::Config::new(Hop::new(hop).unwrap()));
	spawn(driver);
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
	let apply = |live: &mut std::collections::HashSet<String>, event: moq_net::announce::Event| match event {
		moq_net::announce::Event::Start(update)
		| moq_net::announce::Event::Update(update)
		| moq_net::announce::Event::Restart(update) => {
			live.insert(update.prefix.to_string());
		}
		moq_net::announce::Event::End(update) => {
			live.remove(update.prefix.as_str());
		}
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
	/// Each side's extensions, when not the defaults.
	client_extensions: Option<moq_net::setup::Extensions>,
	server_extensions: Option<moq_net::setup::Extensions>,
	version: Option<&'static str>,
	/// The connection credential the client's SETUP presents.
	client_token: Option<&'static [u8]>,
	/// The handle the client builds its session on, when not its own.
	client_auth: Option<auth::Handle>,
}

struct Pair {
	client: Session,
	server: Session,
	client_transport: MockSession,
	server_transport: MockSession,
	requests: Option<auth::Requests>,
	/// The SETUP token the server's handshake read.
	setup_token: Option<moq_net::setup::Token>,
	/// Aborting it drops the server's driver without letting it finish.
	server_driver: moq_clock::sim::JoinHandle<Error>,
}

async fn connect(opts: Options) -> Pair {
	let version: Version = opts.version.unwrap_or(LITE_07).parse().unwrap();
	let mut mock = moq_net::transport::memory::Config::default();
	mock.protocol = Some(version.alpn().to_string());
	let (client_transport, server_transport) = create_mock_session_pair(mock);

	let mut client = Client::new().with_versions(version.into());
	if let Some(token) = opts.client_token {
		client = client.with_token(moq_net::setup::Token {
			kind: moq_net::setup::Token::OUT_OF_BAND,
			value: token.to_vec(),
		});
	}
	if let Some(extensions) = opts.client_extensions {
		client = client.with_extensions(extensions);
	}
	if let Some(publish) = &opts.client_publish {
		client = client.with_publisher(publish);
	}
	if let Some(subscribe) = opts.client_subscribe {
		client = client.with_subscriber(subscribe);
	}
	if let Some(handle) = opts.client_auth {
		client = client.with_auth(handle);
	}

	let mut server = Server::new().with_versions(version.into());
	if let Some(extensions) = opts.server_extensions {
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
	let client_fut = async {
		let (session, driver) = client.connect(now(), client_transport).await.expect("client handshake");
		spawn(driver);
		session
	};
	let server_fut = async {
		let mut handshake = server
			.accept_request(now(), server_transport)
			.await
			.expect("server handshake");
		let setup_token = handshake.token().cloned();
		// Taken before the session starts, the way a relay verifying tokens would.
		let requests = opts.server_requests.then(|| handshake.requests());
		let (session, mut driver) = handshake.ok().await.expect("server accept");
		let driver = moq_clock::sim::spawn(moq_clock::drive(move |now, waiter| {
			moq_net::time::Driver::poll(&mut driver, now, waiter)
		}));
		(session, requests, setup_token, driver)
	};
	let (client, (server, requests, setup_token, server_driver)) = futures::join!(client_fut, server_fut);

	Pair {
		client,
		server,
		client_transport: observe,
		server_transport: observe_server,
		requests,
		setup_token,
		server_driver,
	}
}

/// Answer the peer's tokens from a table, holding every grant (and any request the
/// table leaves unanswered) for as long as the returned task lives.
fn serve(
	mut requests: auth::Requests,
	answer: impl Fn(&[u8]) -> Option<Grant> + 'static,
) -> futures::channel::mpsc::UnboundedReceiver<(Vec<u8>, auth::Issued)> {
	let (tx, rx) = futures::channel::mpsc::unbounded();
	moq_clock::sim::spawn(async move {
		let mut unanswered = Vec::new();
		while let Some(request) = requests.next().await {
			let token = request.token().to_vec();
			match answer(&token) {
				Some(grant) => {
					let _ = tx.unbounded_send((token, request.accept(grant)));
				}
				None => unanswered.push(request),
			}
		}
	});
	rx
}

fn within<F: std::future::Future>(f: F) -> impl std::future::Future<Output = Result<F::Output, moq_clock::Elapsed>> {
	moq_clock::timeout(TEST_TIMEOUT, f)
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

/// A publication waits on a presented token, then is judged against the settled union.
async fn an_unanswered_token_holds_the_check(version: &'static str) {
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
		let mut requests = pair.requests.take().unwrap();
		let _setup = requests.next().await.unwrap().accept(grant(&["baz"], &[]));
		assert_eq!(granted(&pair.client).await, grant(&["baz"], &[]));
		let auth = pair.client.auth();
		let pending = moq_clock::sim::spawn(async move { auth.add("new").await });
		let unanswered = requests.next().await.unwrap();
		let broadcast = publisher.create_broadcast("foo/bar").unwrap();
		broadcast.announce(Default::default()).unwrap();
		moq_clock::sleep(Duration::from_secs(1)).await;
		use futures::FutureExt;
		assert!(
			pair.server.closed().now_or_never().is_none(),
			"publication refused before token answered"
		);
		let _issued = unanswered.accept(grant(&["foo"], &[]));
		let _token = pending.await.unwrap().unwrap();
		assert_eq!(granted(&pair.client).await, grant(&["baz", "foo"], &[]));
		let bad = publisher.create_broadcast("still-bad").unwrap();
		bad.announce(Default::default()).unwrap();
		assert!(matches!(
			pair.server.closed().await,
			Error::Session(SessionError::Unauthorized)
		));
		let (_, reason) = pair.client_transport.close_reason().expect("closed");
		assert_eq!(reason, "unauthorized: still-bad");
	})
	.await
	.expect("timed out");
}

/// A broadcast withdrawn while its token is unanswered is not judged once the token settles.
async fn a_broadcast_withdrawn_while_its_token_waits_is_not_refused(version: &'static str) {
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
		let mut requests = pair.requests.take().unwrap();
		let _setup = requests.next().await.unwrap().accept(grant(&["baz"], &[]));
		assert_eq!(granted(&pair.client).await, grant(&["baz"], &[]));
		let auth = pair.client.auth();
		let pending = moq_clock::sim::spawn(async move { auth.add("new").await });
		let unanswered = requests.next().await.unwrap();
		let withdrawn = publisher.create_broadcast("foo/bar").unwrap();
		withdrawn.announce(Default::default()).unwrap();
		moq_clock::sleep(Duration::from_secs(1)).await;
		withdrawn.unannounce();
		moq_clock::sleep(Duration::from_secs(1)).await;
		let _issued = unanswered.accept(grant(&["other"], &[]));
		let _token = pending.await.unwrap().unwrap();
		assert_eq!(granted(&pair.client).await, grant(&["baz", "other"], &[]));
		// Still enforcing: the next uncovered broadcast is the one named.
		let bad = publisher.create_broadcast("still-bad").unwrap();
		bad.announce(Default::default()).unwrap();
		assert!(matches!(
			pair.server.closed().await,
			Error::Session(SessionError::Unauthorized)
		));
		let (_, reason) = pair.client_transport.close_reason().expect("closed");
		assert_eq!(reason, "unauthorized: still-bad");
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
		let (_, _setup) = issued.next().await.unwrap();

		let (t1, _) = pair.client.auth().add("t1").await.expect("t1 granted");
		let (_, t1_issued) = issued.next().await.unwrap();
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
		let (_, _setup) = issued.next().await.unwrap();
		let (t1, _) = pair.client.auth().add("t1").await.unwrap();
		let (_, t1_issued) = issued.next().await.unwrap();

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
		let (_, setup) = issued.next().await.unwrap();

		let a = publisher.create_broadcast("a/x").unwrap();
		a.announce(Default::default()).unwrap();
		let served = relay.consume();
		wait_announced(&served, "a/x", true).await;

		setup.revoke(SessionError::Unauthorized, "expired");
		wait_for(pair.client.auth().grant(), |g| g == &Some(Grant::default())).await;
		wait_announced(&served, "a/x", false).await;
		// Still published locally, and the session survives the empty union.
		crate::support::announced(&publisher.consume(), "a/x").await;
		assert_eq!(pair.client_transport.close_reason(), None);

		let (_again, _) = pair.client.auth().add("again").await.expect("re-authorized");
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
		moq_clock::sim::spawn(async move {
			while let Some(request) = requests.next().await {
				match request.token().is_empty() {
					true => {
						std::mem::forget(request.accept(Grant::default()));
					}
					false if request.token() == b"opaque".as_slice() => {
						request.reject(SessionError::Unsupported, "unknown token format")
					}
					false => request.reject(SessionError::Unauthorized, "bad signature"),
				}
			}
		});

		let err = pair.client.auth().add("forged").await.expect_err("refused");
		assert!(matches!(err, Error::Session(SessionError::Unauthorized)), "{err:?}");
		let err = pair.client.auth().add("opaque").await.expect_err("refused");
		assert!(matches!(err, Error::Unsupported), "{err:?}");
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
		moq_clock::sim::spawn(async move {
			while let Some(request) = requests.next().await {
				request.reject(SessionError::Unauthorized, "bad credential");
			}
		});

		assert_eq!(granted(&pair.client).await, Grant::default());
	})
	.await
	.expect("timed out");
}

/// The token a client's SETUP presents reaches the server's handshake, and the client's
/// empty-token AUTH stream learns the grant the server gives it.
async fn the_setup_token_is_the_connection_credential(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_requests: true,
			client_token: Some(b"connection-credential"),
			..Default::default()
		})
		.await;
		assert_eq!(
			pair.setup_token.map(|token| (token.kind, token.value)),
			Some((moq_net::setup::Token::OUT_OF_BAND, b"connection-credential".to_vec()))
		);

		let _issued = serve(pair.requests.take().unwrap(), |token| {
			token.is_empty().then(|| grant(&["room"], &[]))
		});
		assert_eq!(granted(&pair.client).await, grant(&["room"], &[]));
	})
	.await
	.expect("timed out");
}

/// A lite version without the Token parameter carries none, so the caller's URL query
/// has to.
#[moq_clock::test]
async fn lite_06_sends_no_setup_token() {
	within(async {
		let pair = connect(Options {
			version: Some(LITE_06),
			client_token: Some(b"connection-credential"),
			..Default::default()
		})
		.await;
		assert_eq!(pair.setup_token, None);
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
		let pending = moq_clock::sim::spawn(async move { auth.add("queued").await.map(drop) });
		// Give the second token time to reach the queue before dropping it.
		moq_clock::sleep(Duration::from_millis(50)).await;
		drop(requests);

		let err = pending.await.unwrap().expect_err("refused");
		assert!(matches!(err, Error::Session(SessionError::Unauthorized)), "{err:?}");
	})
	.await
	.expect("timed out");
}

/// A closed session holds no grant: no session is live, and every token ended with it.
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
		wait_for(token, Option::is_none).await;
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

		let err = pair.client.auth().add("token").await.expect_err("no acceptor");
		assert!(matches!(err, Error::Unsupported), "{err:?}");
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

/// Versions without AUTH never open the stream: no grant. moq-transport keeps an added
/// token, with a grant that stays unknown, for the requests that name it; moq-lite has
/// nothing that could carry one.
async fn older_versions_have_no_grant(version: &'static str) {
	within(async {
		let (pair, _broadcast) = connect_announced(version).await;
		assert_eq!(pair.client.auth().grant().peek(), None);
		match version.starts_with("moq-lite") {
			true => assert!(matches!(pair.client.auth().add("token").await, Err(Error::Unsupported))),
			false => {
				let (auth, _) = pair.client.auth().add("token").await.expect("kept for requests");
				assert_eq!(auth.grant().peek(), None);
				assert_eq!(pair.client.auth().grant().peek(), None);
			}
		}
		// moq-transport's request streams have no stream type to look for.
		if version.starts_with("moq-lite") {
			for transport in [&pair.client_transport, &pair.server_transport] {
				assert!(
					!transport.bidi_types().contains(&AUTH_STREAM),
					"{:x?}",
					transport.bidi_types()
				);
			}
		}
	})
	.await
	.expect("timed out");
}

/// An exact path and a subtree are both admitted, and told as issued wherever AUTH runs:
/// a grant is never refused for the shape of its patterns.
async fn exact_and_subtree_grants_admit_their_broadcast(version: &'static str) {
	for scope in ["ch1", "ch1/**"] {
		within(async {
			let relay = produce_origin(1);
			let broadcast = relay.create_broadcast("ch1").unwrap();
			let track = broadcast.create_track("video", timed()).unwrap();
			broadcast.announce(Default::default()).unwrap();

			let received = produce_origin(2);
			let scope = pattern_set(&[scope]);
			let pair = connect(Options {
				version: Some(version),
				client_subscribe: Some(received.clone()),
				server_publish: Some(relay.scope("", &scope).unwrap()),
				..Default::default()
			})
			.await;

			let mut group = track.append_group().unwrap();
			group
				.write_frame(moq_net::Timestamp::from_millis(0).unwrap(), b"frame".as_ref())
				.unwrap();
			let remote = received
				.consume()
				.follow("ch1", None)
				.unwrap()
				.available()
				.await
				.unwrap();
			let prefs = moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10));
			let mut sub = remote.track("video").unwrap().subscribe(prefs).await.unwrap();
			sub.recv_group().await.unwrap().expect("a group");

			assert_eq!(granted(&pair.client).await.subscribe, scope);
			assert_eq!(pair.client_transport.close_reason(), None);
		})
		.await
		.expect("timed out");
	}
}

/// Connect on `version` and wait for a client announcement to reach the server. Each
/// driver presents its setup token before it handles any other stream, so by then a
/// version with AUTH has opened its Auth Stream on both sides.
async fn connect_announced(version: &'static str) -> (Pair, moq_net::origin::Publication) {
	let client_origin = produce_origin(2);
	let broadcast = client_origin.create_broadcast("room/x").unwrap();
	broadcast.announce(Default::default()).unwrap();
	let server_origin = produce_origin(1);
	let pair = connect(Options {
		version: Some(version),
		client_publish: Some(client_origin),
		server_subscribe: Some(server_origin.clone()),
		..Default::default()
	})
	.await;
	wait_announced(&server_origin.consume(), "room/x", true).await;
	(pair, broadcast)
}

async fn a_live_subscription_survives_union_renewal(version: &'static str) {
	within(async {
		let publisher = produce_origin(1);
		let broadcast = publisher.create_broadcast("room/x").unwrap();
		let published = broadcast.create_track("video", timed()).unwrap();
		broadcast.announce(Default::default()).unwrap();
		let received = produce_origin(2);
		let mut pair = connect(Options {
			version: Some(version),
			client_subscribe: Some(received.clone()),
			server_publish: Some(publisher),
			server_requests: true,
			..Default::default()
		})
		.await;
		pair.server.auth().authorize(&Grant::default());
		let mut requests = pair.requests.take().unwrap();
		let _setup = requests.next().await.unwrap().accept(Grant::default());
		let auth = pair.client.auth();
		let (first, first_grant) = futures::join!(auth.add("first"), async {
			requests.next().await.unwrap().accept(grant(&[], &["room"]))
		});
		let first = first.unwrap();
		let remote = received
			.consume()
			.follow("room/x", None)
			.unwrap()
			.available()
			.await
			.unwrap();
		let mut group = published.append_group().unwrap();
		group
			.write_frame(moq_net::Timestamp::from_millis(0).unwrap(), b"first".as_ref())
			.unwrap();
		let mut subscription = remote
			.track("video")
			.unwrap()
			.subscribe(moq_net::track::Subscription::default())
			.await
			.unwrap();
		subscription.recv_group().await.unwrap().unwrap();
		let (renewed, _renewed_grant) = futures::join!(auth.add("renewal"), async {
			requests.next().await.unwrap().accept(grant(&[], &["room"]))
		});
		let _renewed = renewed.unwrap();
		drop(first);
		first_grant.closed().await;
		let mut group = published.append_group().unwrap();
		group
			.write_frame(moq_net::Timestamp::from_millis(1).unwrap(), b"renewed".as_ref())
			.unwrap();
		subscription
			.recv_group()
			.await
			.expect("renewal ended the subscription")
			.unwrap();
	})
	.await
	.expect("timed out");
}

/// Losing a grant cancels the subscriptions it covered, in both directions, and
/// leaves the session up.
async fn a_revoked_grant_cancels_its_subscriptions(version: &'static str) {
	within(async {
		let ts = |ms| moq_net::Timestamp::from_millis(ms).unwrap();
		let prefs = || moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10));

		// The server publishes room/x to the client; the client publishes up/y to the server.
		let server_origin = produce_origin(1);
		let down = server_origin.create_broadcast("room/x").unwrap();
		let down_track = down.create_track("video", timed()).unwrap();
		down.announce(Default::default()).unwrap();

		let client_origin = produce_origin(2);
		let up = client_origin.create_broadcast("up/y").unwrap();
		let up_track = up.create_track("video", timed()).unwrap();
		up.announce(Default::default()).unwrap();

		let received = produce_origin(3);
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(client_origin.clone()),
			client_subscribe: Some(received.clone()),
			server_publish: Some(server_origin.clone()),
			server_subscribe: Some(server_origin.clone()),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut issued = serve(pair.requests.take().unwrap(), |token| {
			token.is_empty().then(|| grant(&["up"], &["room"]))
		});
		let (_, setup) = issued.next().await.unwrap();

		let mut group = down_track.append_group().unwrap();
		group.write_frame(ts(0), b"down".as_ref()).unwrap();
		let mut group = up_track.append_group().unwrap();
		group.write_frame(ts(0), b"up".as_ref()).unwrap();

		let remote = received
			.consume()
			.follow("room/x", None)
			.unwrap()
			.available()
			.await
			.unwrap();
		let mut down_sub = remote.track("video").unwrap().subscribe(prefs()).await.unwrap();
		down_sub.recv_group().await.unwrap().unwrap();

		let remote = server_origin
			.consume()
			.follow("up/y", None)
			.unwrap()
			.available()
			.await
			.unwrap();
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
		if version == LITE_07 {
			assert!(matches!(err, Error::Stream(StreamError::Unauthorized)), "{err:?}");
		}
		assert_eq!(pair.client_transport.close_reason(), None);

		// Neither stream claims the session closed. moq-lite resets both with UNAUTHORIZED;
		// moq-transport has no such stream code and reports it on the request instead.
		let resets = pair.client_transport.resets();
		let closed = StreamError::Session(SessionError::Unauthorized).to_code();
		assert!(!resets.contains(&closed), "{resets:x?}");
		if version == LITE_07 {
			let unauthorized = StreamError::Unauthorized.to_code();
			let count = resets.iter().filter(|&&code| code == unauthorized).count();
			assert_eq!(count, 2, "both subscriptions reset with UNAUTHORIZED: {resets:x?}");
		}
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
		let (_, _setup) = issued.next().await.unwrap();
		assert_eq!(granted(&pair.client).await, grant(&["a"], &[]));

		let err = pair.client.auth().add("huge").await.expect_err("too large");
		assert!(matches!(err, Error::Unsupported), "{err:?}");
		// No AUTH_OK for it reached the wire: the union is still just the setup grant.
		assert_eq!(pair.client.auth().grant().peek(), Some(grant(&["a"], &[])));
		assert_eq!(pair.client_transport.close_reason(), None);
	})
	.await
	.expect("timed out");
}

async fn a_grant_update_too_large_for_one_message_is_unsupported(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_subscribe: Some(produce_origin(1)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut issued = serve(pair.requests.take().unwrap(), |token| {
			Some(grant(if token.is_empty() { &["a"] } else { &["b"] }, &[]))
		});
		let (_, _setup) = issued.next().await.unwrap();
		let (token, _) = pair.client.auth().add("update").await.unwrap();
		let (_, updated) = issued.next().await.unwrap();
		assert_eq!(granted(&pair.client).await, grant(&["a", "b"], &[]));
		let huge: Patterns = (0..20)
			.map(|i| Pattern::subtree(&format!("{i}{}", "x".repeat(4000))).unwrap())
			.collect();
		updated.update(Grant {
			publish: huge,
			subscribe: Patterns::new(),
			expires: None,
		});
		let err = token.closed().await;
		assert!(matches!(err, Error::Unsupported), "{err:?}");
		assert_eq!(token.grant().peek(), None);
		wait_for(pair.client.auth().grant(), |g| g == &Some(grant(&["a"], &[]))).await;
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
		let (_, _setup) = issued.next().await.unwrap();
		assert_eq!(granted(&pair.client).await, table(b"").unwrap());

		let mut held = Vec::new();
		for token in ["exact", "wildcard", "mixed", "root"] {
			let (added, _) = pair.client.auth().add(token).await.expect(token);
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
		let leaked = moq_clock::timeout(
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

/// One origin both publishing and consuming holds what the peer sent us, which is not a
/// publication of ours: only what we publish is judged against the grant.
async fn a_broadcast_received_from_the_peer_is_not_ours_to_check(version: &'static str) {
	within(async {
		let local = produce_origin(2);
		let relay = produce_origin(1);
		let remote = relay.create_broadcast("remote/x").unwrap();
		remote.announce(Default::default()).unwrap();

		let pair = connect(Options {
			version: Some(version),
			client_publish: Some(local.clone()),
			client_subscribe: Some(local.clone()),
			server_publish: Some(relay.clone()),
			server_subscribe: Some(relay.scope("", &patterns(&["local"])).unwrap()),
			..Default::default()
		})
		.await;
		assert_eq!(granted(&pair.client).await, grant(&["local"], &[""]));

		wait_announced(&local.consume(), "remote/x", true).await;

		// The first broadcast refused is the one we published, not the one we received.
		let bad = local.create_broadcast("bad/x").unwrap();
		bad.announce(Default::default()).unwrap();
		assert!(matches!(
			pair.server.closed().await,
			Error::Session(SessionError::Unauthorized)
		));
		let (_, reason) = pair.client_transport.close_reason().expect("closed");
		assert_eq!(reason, "unauthorized: bad/x");
	})
	.await
	.expect("timed out");
}

/// Run each limit case on every version family: the session enforces its limit itself,
/// so a peer without AUTH (or one that ignores it) cannot keep what it lost.
macro_rules! limit_cases {
	($($case:ident),* $(,)?) => {
		mod limit_lite_05 {
			$(#[moq_clock::test] async fn $case() { super::$case("moq-lite-05").await })*
		}
		mod limit_lite_06 {
			$(#[moq_clock::test] async fn $case() { super::$case(super::LITE_06).await })*
		}
		mod limit_lite_07 {
			$(#[moq_clock::test] async fn $case() { super::$case(super::LITE_07).await })*
		}
		mod limit_moqt_18 {
			$(#[moq_clock::test] async fn $case() { super::$case(super::MOQT_18).await })*
		}
	};
}

limit_cases!(
	a_narrowing_deafens_one_path,
	a_narrowing_aborts_what_the_peer_published,
	a_widening_brings_back_a_deafened_path,
	a_widening_brings_back_what_the_peer_published,
);

#[moq_clock::test]
async fn lite_05_narrowing_resets_a_fetch_in_flight() {
	a_narrowing_resets_a_fetch_in_flight("moq-lite-05").await
}

#[moq_clock::test]
async fn lite_06_narrowing_resets_a_fetch_in_flight() {
	a_narrowing_resets_a_fetch_in_flight(LITE_06).await
}

#[moq_clock::test]
async fn lite_07_narrowing_resets_a_fetch_in_flight() {
	a_narrowing_resets_a_fetch_in_flight(LITE_07).await
}

/// Whether `version` exchanges AUTH, so the peer also hears of a narrowing.
fn speaks_auth(version: &str) -> bool {
	matches!(version, LITE_07 | MOQT_18 | MOQT_22)
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
		let prefs = || moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10));

		let relay = produce_origin(1);
		let audio = relay.create_broadcast("room/alice/audio").unwrap();
		let audio_track = audio.create_track("opus", timed()).unwrap();
		audio.announce(Default::default()).unwrap();
		let video = relay.create_broadcast("room/alice/video").unwrap();
		let video_track = video.create_track("h264", timed()).unwrap();
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

		let remote = received
			.consume()
			.follow("room/alice/audio", None)
			.unwrap()
			.available()
			.await
			.unwrap();
		let mut audio_sub = remote.track("opus").unwrap().subscribe(prefs()).await.unwrap();
		audio_sub.recv_group().await.unwrap().unwrap();
		let remote = received
			.consume()
			.follow("room/alice/video", None)
			.unwrap()
			.available()
			.await
			.unwrap();
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
		// UNAUTHORIZED, even on a version without the Auth Stream, and neither side
		// closed the session.
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
		let prefs = || moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10));

		let client_origin = produce_origin(2);
		let mic = client_origin.create_broadcast("room/bob/mic").unwrap();
		let mic_track = mic.create_track("opus", timed()).unwrap();
		mic.announce(Default::default()).unwrap();
		let cam = client_origin.create_broadcast("room/bob/cam").unwrap();
		let cam_track = cam.create_track("h264", timed()).unwrap();
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

		let remote = relay
			.consume()
			.follow("room/bob/mic", None)
			.unwrap()
			.available()
			.await
			.unwrap();
		let mut mic_sub = remote.track("opus").unwrap().subscribe(prefs()).await.unwrap();
		mic_sub.recv_group().await.unwrap().unwrap();
		let remote = relay
			.consume()
			.follow("room/bob/cam", None)
			.unwrap()
			.available()
			.await
			.unwrap();
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
		let track = broadcast.create_track("video", timed()).unwrap();
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

		let remote = received
			.consume()
			.follow("room/x", None)
			.unwrap()
			.available()
			.await
			.unwrap();
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
		// The narrowing also retracts the route, and a fetch-only track whose route
		// leaves ends `Unroutable` locally, so the reader may see either. The wire carries
		// the revocation regardless.
		assert!(
			unauthorized(&err) || matches!(err, moq_net::Error::Unroutable),
			"{err:?}"
		);
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
		let prefs = || moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10));

		let relay = produce_origin(1);
		let audio = relay.create_broadcast("room/alice/audio").unwrap();
		let audio_track = audio.create_track("opus", timed()).unwrap();
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
		let remote = received
			.consume()
			.follow("room/alice/audio", None)
			.unwrap()
			.available()
			.await
			.unwrap();
		let mut sub = remote.track("opus").unwrap().subscribe(prefs()).await.unwrap();
		sub.recv_group().await.unwrap().unwrap();

		pair.server.auth().authorize(&grant(&[], &["room/alice/video"]));
		let err = ended(&mut sub).await;
		assert!(unauthorized(&err), "{err:?}");
		wait_announced(&received.consume(), "room/alice/audio", false).await;
		drop((sub, remote));

		pair.server.auth().authorize(&grant(&[], &["room"]));
		wait_announced(&received.consume(), "room/alice/audio", true).await;
		let remote = received
			.consume()
			.follow("room/alice/audio", None)
			.unwrap()
			.available()
			.await
			.unwrap();
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
		let prefs = || moq_net::track::Subscription::default().with_max_delay(Duration::from_secs(10));

		let client_origin = produce_origin(2);
		let mic = client_origin.create_broadcast("room/bob/mic").unwrap();
		let mic_track = mic.create_track("opus", timed()).unwrap();
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
		let remote = relay
			.consume()
			.follow("room/bob/mic", None)
			.unwrap()
			.available()
			.await
			.unwrap();
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

/// A handle passed to the client presents its token on every session it backs: added before
/// any session, granted on the first, cleared while disconnected, and granted again on the
/// next, without the token ending in between.
async fn a_shared_handle_presents_its_token_on_every_session(version: &'static str) {
	within(async {
		let handle = auth::Handle::new();
		let added = moq_clock::sim::spawn({
			let handle = handle.clone();
			async move { handle.add("token").await }
		});
		let options = || Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_requests: true,
			client_auth: Some(handle.clone()),
			..Default::default()
		};

		let mut first = connect(options()).await;
		let mut tokens = serve(first.requests.take().unwrap(), |token| match token {
			b"" => Some(Grant::default()),
			_ => Some(grant(&["first"], &[])),
		});
		let (token, answer) = added.await.expect("joined").expect("granted on the first session");
		assert_eq!(answer, auth::Answer::Granted);
		let _first = presented(&mut tokens).await;
		wait_for(token.grant(), |g| g == &Some(grant(&["first"], &[]))).await;

		first.server.abort(Error::Cancel);
		first.client.closed().await;
		assert_eq!(token.grant().peek(), None, "no session is live");
		assert!(
			futures::FutureExt::now_or_never(token.closed()).is_none(),
			"a session ending is no verdict"
		);

		let mut second = connect(options()).await;
		let mut tokens = serve(second.requests.take().unwrap(), |token| match token {
			b"" => Some(Grant::default()),
			_ => Some(grant(&["second"], &[])),
		});
		let _second = presented(&mut tokens).await;
		wait_for(token.grant(), |g| g == &Some(grant(&["second"], &[]))).await;
		wait_for(handle.grant(), |g| g == &Some(grant(&["second"], &[]))).await;
	})
	.await
	.expect("timed out");
}

/// The setup token and the handle's token, in either order, once both are presented.
async fn presented(
	tokens: &mut futures::channel::mpsc::UnboundedReceiver<(Vec<u8>, auth::Issued)>,
) -> Vec<(Vec<u8>, auth::Issued)> {
	let mut both = vec![
		tokens.next().await.expect("a token"),
		tokens.next().await.expect("another token"),
	];
	both.sort_by(|a, b| a.0.cmp(&b.0));
	assert_eq!(both[0].0, b"");
	assert_eq!(both[1].0, b"token");
	both
}

/// A handle backs one active session: another fails loudly until the first drains. The
/// draining session keeps presenting the handle's tokens beside its successor, even one
/// its peer has not answered yet, since its requests may still rely on them. A token added
/// after the drain rides only the successor: a draining session opens no new streams.
async fn a_handle_backs_one_active_session(version: &'static str) {
	within(async {
		use futures::FutureExt;

		let handle = auth::Handle::new();
		let options = || Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_requests: true,
			client_auth: Some(handle.clone()),
			..Default::default()
		};
		let mut first = connect(options()).await;
		let mut first_requests = first.requests.take().unwrap();
		let added = moq_clock::sim::spawn({
			let handle = handle.clone();
			async move { handle.add("token").await }
		});
		let mut first_issued = Vec::new();
		let unanswered = loop {
			let request = first_requests.next().await.expect("a token");
			if request.token().is_empty() {
				first_issued.push(request.accept(Grant::default()));
				continue;
			}
			assert_eq!(request.token().as_ref(), b"token");
			break request;
		};

		let parsed: Version = version.parse().unwrap();
		let mut mock = moq_net::transport::memory::Config::default();
		mock.protocol = Some(parsed.alpn().to_string());
		let (transport, _peer) = create_mock_session_pair(mock);
		let concurrent = Client::new()
			.with_versions(parsed.into())
			.with_auth(handle.clone())
			.connect(now(), transport)
			.await;
		assert!(matches!(concurrent, Err(Error::Duplicate)), "{:?}", concurrent.err());

		first.server.drain().send(moq_net::goaway::Goaway::new()).unwrap();
		support::Draining::recv(&first.client.draining())
			.await
			.expect("the GOAWAY");
		let mut second = connect(options()).await;
		let mut second_tokens = serve(second.requests.take().unwrap(), |_| Some(Grant::all()));
		let _second_issued = presented(&mut second_tokens).await;
		let _token = added.await.expect("joined").expect("granted on the second session");

		moq_clock::sleep(Duration::from_secs(1)).await;
		assert!(
			unanswered.closed().now_or_never().is_none(),
			"the draining session withdrew its unanswered token"
		);
		let first_grant = unanswered.accept(Grant::all());
		moq_clock::sleep(Duration::from_secs(1)).await;
		assert!(
			first_grant.closed().now_or_never().is_none(),
			"the draining session dropped its late grant"
		);

		let (later, _) = handle.add("later").await.expect("granted on the second session");
		let (token, _later) = second_tokens.next().await.expect("the active session presents it");
		assert_eq!(token, b"later");
		assert!(
			first_requests.next().now_or_never().is_none(),
			"the draining session opened a new stream"
		);
		drop(later);
	})
	.await
	.expect("timed out");
}

/// A token queued while the session is active, but dequeued only after this side sent a
/// GOAWAY, opens no request on the draining session: its successor presents it.
async fn a_token_queued_before_our_goaway_rides_only_the_successor(version: &'static str) {
	within(async {
		use futures::FutureExt;

		let handle = auth::Handle::new();
		let options = || Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_requests: true,
			client_auth: Some(handle.clone()),
			..Default::default()
		};
		let mut first = connect(options()).await;
		let mut first_tokens = serve(first.requests.take().unwrap(), |_| Some(Grant::all()));
		let (setup, _setup) = first_tokens.next().await.expect("the setup token");
		assert!(setup.is_empty());

		// Its first poll queues the token; the driver runs only once this side drains.
		let mut adding = Box::pin(handle.add("late"));
		assert!(adding.as_mut().now_or_never().is_none());
		first.client.drain().send(moq_net::goaway::Goaway::new()).unwrap();
		moq_clock::sleep(Duration::from_secs(1)).await;
		assert!(
			first_tokens.next().now_or_never().is_none(),
			"the draining session presented a new token"
		);

		let mut second = connect(options()).await;
		let mut second_tokens = serve(second.requests.take().unwrap(), |_| Some(Grant::all()));
		let mut tokens = vec![
			second_tokens.next().await.expect("a token").0,
			second_tokens.next().await.expect("another token").0,
		];
		tokens.sort();
		assert_eq!(tokens, [b"".to_vec(), b"late".to_vec()]);
		let (_late, answer) = adding.await.expect("granted on the successor");
		assert_eq!(answer, auth::Answer::Granted);
	})
	.await
	.expect("timed out");
}

/// A refusal that may pass, such as an overloaded verifier, presents the token again
/// after a backoff rather than ending it.
async fn a_transient_refusal_is_presented_again(version: &'static str) {
	within(async {
		let mut pair = connect(Options {
			version: Some(version),
			client_publish: Some(produce_origin(2)),
			server_requests: true,
			..Default::default()
		})
		.await;
		let mut requests = pair.requests.take().unwrap();
		let (tx, mut issued) = futures::channel::mpsc::unbounded();
		moq_clock::sim::spawn(async move {
			let mut refused = false;
			while let Some(request) = requests.next().await {
				if request.token().is_empty() || refused {
					let _ = tx.unbounded_send(request.accept(Grant::default()));
				} else {
					refused = true;
					request.reject(SessionError::TooManyRequests, "busy");
				}
			}
		});
		let (token, answer) = pair.client.auth().add("token").await.expect("granted on the retry");
		assert_eq!(answer, auth::Answer::Granted);
		assert_eq!(token.grant().peek(), Some(Grant::default()));
		let _setup = issued.next().await;
		let _token = issued.next().await;
	})
	.await
	.expect("timed out");
}
