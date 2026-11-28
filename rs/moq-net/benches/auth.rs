//! AUTH admission bursts across sessions and tokens per session, without sockets.
//!
//! Measures the pending queue and complete grant/withdrawal lifecycle. Run the same
//! matrix before and after queue changes so a per-session scan appears on its own axis.
//!
//! `renewal` times swapping the auth of every announced route on one origin, until
//! the session that presented both auths has renewed each announcement and its peer
//! accepted every token. `renewal_one` changes one route while the others remain idle.
//! Both sweep the routes and the sessions publishing that origin:
//! the others carry no auth for those routes, so their cost is waking on a change that
//! sends nothing.
//!
//! Run with `cargo bench -p moq-net --features async --bench auth`.

#[path = "../tests/integration/support/mod.rs"]
mod support;

use std::{fmt, time::Duration};

use bytes::Bytes;
use futures::future::join_all;
use moq_net::{Session, Version, auth, origin};
use support::{harness, mock::create_mock_session_pair};
use swole::bench;

/// Two swept counts, named `<outer>/<inner>`: sessions and tokens, sessions and routes,
/// tokens and paths, or tokens and naming requests.
#[derive(Clone, Copy)]
struct Case(usize, usize);

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		write!(f, "{}/{}", self.0, self.1)
	}
}

fn cases(outer: [usize; 3], inner: [usize; 3]) -> Vec<Case> {
	outer.into_iter().flat_map(|a| inner.map(|b| Case(a, b))).collect()
}

fn runtime() -> tokio::runtime::Runtime {
	tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.unwrap()
}

/// An origin that forwards every change at once, so a call times the work rather than the
/// origin batching it.
fn unheld(rt: &tokio::runtime::Runtime) -> origin::Producer {
	let _guard = rt.enter();
	let config = origin::Config::new(std::num::NonZeroU64::new(2).unwrap()).with_update_hold(Duration::ZERO);
	let (origin, driver) = origin::Producer::new(config);
	harness::spawn(driver);
	origin
}

/// The one version that presents tokens in band: on moq-transport a token resolves at once
/// and only rides the requests that name it, so no AUTH request ever reaches the peer.
const INBAND: &str = "moq-lite-07-wip";

struct Pair {
	client: Session,
	_server: Session,
	requests: auth::Requests,
	_initial: auth::Issued,
}

struct Origins<'a> {
	publish: &'a origin::Producer,
	subscribe: origin::Producer,
}

async fn pair(origins: Option<Origins<'_>>) -> Pair {
	let version: Version = INBAND.parse().unwrap();
	let mut mock = moq_net::transport::memory::Config::default();
	mock.protocol = Some(version.alpn().to_string());
	let (client, server) = create_mock_session_pair(mock);
	let client = async {
		let mut config = moq_net::Client::new().with_versions(version.into());
		if let Some(origins) = &origins {
			config = config.with_publisher(origins.publish);
		}
		let (session, driver) = config.connect(harness::now(), client).await.unwrap();
		harness::spawn(driver);
		session
	};
	let server = async {
		let mut config = moq_net::Server::new().with_versions(version.into());
		if let Some(origins) = &origins {
			config = config.with_subscriber(origins.subscribe.clone());
		}
		let mut request = config.handshake(harness::now(), server).await.unwrap();
		let requests = request.requests();
		let (session, driver) = request.ok().await.unwrap();
		harness::spawn(driver);
		(session, requests)
	};
	let (client, (server, mut requests)) = futures::join!(client, server);
	let initial = requests.next().await.unwrap().accept(match origins {
		Some(_) => auth::Grant::default(),
		None => auth::Grant::all(),
	});
	Pair {
		client,
		_server: server,
		requests,
		_initial: initial,
	}
}

impl Pair {
	async fn round(&mut self, count: usize) {
		let handle = self.client.auth();
		let present = join_all((0..count).map(|_| added(&handle, Bytes::from_static(b"token"))));
		let accept = async {
			let mut issued = Vec::with_capacity(count);
			for _ in 0..count {
				issued.push(self.requests.next().await.unwrap().accept(auth::Grant::all()));
			}
			issued
		};
		let (tokens, issued) = futures::join!(present, accept);
		let tokens: Vec<_> = tokens.into_iter().map(Result::unwrap).collect();
		drop(tokens);
		join_all(issued.iter().map(auth::Issued::closed)).await;
	}
}

/// Sessions whose every call presents and withdraws `tokens` tokens. Declared before the
/// runtime so they drop while it still runs.
struct Admission {
	pairs: Vec<Pair>,
	tokens: usize,
	rt: tokio::runtime::Runtime,
}

fn admitting(Case(sessions, tokens): Case) -> Admission {
	let rt = runtime();
	let pairs = rt.block_on(join_all((0..sessions).map(|_| pair(None))));
	Admission { pairs, tokens, rt }
}

#[bench(args = cases([1, 16, 64], [1, 16, 128]), setup = admitting, items = |Case(sessions, tokens)| sessions * tokens)]
fn admission(input: &mut Admission) {
	let tokens = input.tokens;
	input
		.rt
		.block_on(join_all(input.pairs.iter_mut().map(|pair| pair.round(tokens))));
}

/// A client publishing `origin` to its own server, whose session and token requests follow.
async fn publisher(origin: &origin::Producer) -> (Session, Session, auth::Requests, origin::Producer) {
	let version: Version = "moq-transport-22".parse().unwrap();
	let mut mock = moq_net::transport::memory::Config::default();
	mock.protocol = Some(version.alpn().to_string());
	let (client, server) = create_mock_session_pair(mock);
	// Tokens ride requests alone, and the client announces with PUBLISH_NAMESPACE, as to a
	// peer without MoQ Solicit.
	let (subscribe, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(1).unwrap()));
	harness::spawn(driver);
	let client = async {
		let (session, driver) = moq_net::Client::new()
			.with_versions(version.into())
			.with_publisher(origin.consume())
			.connect(harness::now(), client)
			.await
			.unwrap();
		harness::spawn(driver);
		session
	};
	let server = async {
		let mut request = moq_net::Server::new()
			.with_versions(version.into())
			.with_solicit(false)
			.with_subscriber(subscribe.clone())
			.handshake(harness::now(), server)
			.await
			.unwrap();
		let requests = request.requests();
		let (session, driver) = request.ok().await.unwrap();
		harness::spawn(driver);
		(session, requests)
	};
	let (client, (server, requests)) = futures::join!(client, server);
	(client, server, requests, subscribe)
}

/// One origin's routes, each holding one of two auths from the first session, which each
/// call swaps on `touched` of them.
struct Renewal {
	_origin: origin::Producer,
	publishers: Vec<(Session, Session, auth::Requests, origin::Producer)>,
	auths: Vec<[auth::Credential; 2]>,
	broadcasts: Vec<origin::Route>,
	/// Each round's grants replace the last round's, which drop once their renewals are accepted.
	held: Vec<auth::Issued>,
	next: usize,
	touched: usize,
	rt: tokio::runtime::Runtime,
}

impl Renewal {
	fn new(Case(sessions, routes): Case, single: bool) -> Self {
		let rt = runtime();
		let origin = unheld(&rt);
		let mut publishers = {
			let _guard = rt.enter();
			rt.block_on(join_all((0..sessions).map(|_| publisher(&origin))))
		};
		let (session, _, requests, _) = &mut publishers[0];
		let handle = session.auth();
		let auths: Vec<_> = rt.block_on(async {
			let mut auths = Vec::new();
			for route in 0..routes {
				auths.push([
					added(&handle, format!("a/{route}")).await.unwrap(),
					added(&handle, format!("b/{route}")).await.unwrap(),
				]);
			}
			auths
		});
		let broadcasts = {
			let _guard = rt.enter();
			(0..routes)
				.map(|route| {
					origin
						.route(
							format!("room/{route}").as_str(),
							origin::RouteOptions::default().with_auth(auths[route][0].clone()),
						)
						.unwrap()
				})
				.collect()
		};
		let held = rt.block_on(accept(requests, routes));
		Self {
			_origin: origin,
			publishers,
			auths,
			broadcasts,
			held,
			next: 1,
			touched: if single { 1 } else { routes },
			rt,
		}
	}

	fn swap(&mut self) {
		let _guard = self.rt.enter();
		for (index, broadcast) in self.broadcasts.iter().take(self.touched).enumerate() {
			broadcast.set_auth(Some(self.auths[index][self.next].clone())).unwrap();
		}
		let replacements = self.rt.block_on(accept(&mut self.publishers[0].2, self.touched));
		drop(self.held.splice(..self.touched, replacements));
		self.next ^= 1;
	}
}

/// Accept the next `count` token requests.
async fn accept(requests: &mut auth::Requests, count: usize) -> Vec<auth::Issued> {
	let mut issued = Vec::with_capacity(count);
	for _ in 0..count {
		issued.push(requests.next().await.unwrap().accept(auth::Grant::all()));
	}
	issued
}

fn renewing(case: Case) -> Renewal {
	Renewal::new(case, false)
}

fn renewing_one(case: Case) -> Renewal {
	Renewal::new(case, true)
}

#[bench(args = cases([1, 16, 64], [1, 16, 128]), setup = renewing, items = |Case(_, routes)| routes)]
fn renewal(input: &mut Renewal) {
	input.swap()
}

#[bench(args = cases([1, 16, 64], [1, 16, 128]), setup = renewing_one, items = |_: Case| 1)]
fn renewal_one(input: &mut Renewal) {
	input.swap()
}

/// Publications held behind unanswered tokens, sweeping both the token table and paths.
struct Waiting {
	publish: origin::Producer,
	receive: origin::Producer,
	pair: Pair,
	generation: usize,
	tokens: usize,
	paths: usize,
	rt: tokio::runtime::Runtime,
}

fn waiting_setup(Case(tokens, paths): Case) -> Waiting {
	let rt = runtime();
	let publish = unheld(&rt);
	let receive = {
		let _guard = rt.enter();
		let (receive, driver) = origin::Producer::new(origin::Config::new(std::num::NonZeroU64::new(1).unwrap()));
		harness::spawn(driver);
		receive
	};
	let pair = rt.block_on(pair(Some(Origins {
		publish: &publish,
		subscribe: receive.clone(),
	})));
	Waiting {
		publish,
		receive,
		pair,
		generation: 0,
		tokens,
		paths,
		rt,
	}
}

#[bench(args = cases([1, 16, 128], [1, 16, 128]), setup = waiting_setup, items = |Case(_, paths)| paths)]
fn waiting(input: &mut Waiting) {
	let Waiting {
		publish,
		receive,
		pair,
		generation,
		tokens,
		paths,
		rt,
	} = input;
	let (tokens, paths) = (*tokens, *paths);
	rt.block_on(async {
		let handle = pair.client.auth();
		let mut presenting = std::pin::pin!(join_all((0..tokens).map(|_| added(&handle, "pending"))));
		assert!(futures::poll!(presenting.as_mut()).is_pending());
		let mut pending = Vec::with_capacity(tokens);
		for _ in 0..tokens {
			pending.push(pair.requests.next().await.unwrap());
		}
		let prefix = format!("wait/{generation}/");
		*generation += 1;
		let broadcasts: Vec<_> = (0..paths)
			.map(|path| {
				let broadcast = publish
					.route(
						format!("{prefix}{path}").as_str(),
						moq_net::origin::RouteOptions::hidden(),
					)
					.unwrap();
				broadcast.announce().unwrap();
				broadcast
			})
			.collect();
		// Let the session reach its admission wait before the final reply widens it.
		tokio::task::yield_now().await;
		let mut issued = Vec::with_capacity(tokens);
		for (index, request) in pending.into_iter().enumerate() {
			issued.push(request.accept(if index + 1 == tokens {
				auth::Grant::all()
			} else {
				auth::Grant::default()
			}));
		}
		let tokens: Vec<_> = presenting.await.into_iter().map(Result::unwrap).collect();
		let mut announced = receive.consume().routes();
		let mut seen = std::collections::HashSet::new();
		while seen.len() < paths {
			match announced.next().await.unwrap() {
				moq_net::origin::RouteEvent {
					kind:
						moq_net::origin::RouteKind::Start
						| moq_net::origin::RouteKind::Update
						| moq_net::origin::RouteKind::Restart,
					announce: update,
					..
				} if update.prefix.as_str().starts_with(&prefix) => {
					seen.insert(update.prefix);
				}
				_ => {}
			}
		}
		drop(broadcasts);
		drop(tokens);
		join_all(issued.iter().map(auth::Issued::closed)).await;
	})
}

/// Request-token lookup and shared withdrawal, sweeping distinct tokens and naming requests.
struct RequestTokens {
	origin: origin::Producer,
	_client: Session,
	_server: Session,
	pending: auth::Requests,
	_receive: origin::Producer,
	announced: origin::Routes,
	auths: Vec<auth::Credential>,
	generation: usize,
	requests: usize,
	rt: tokio::runtime::Runtime,
}

fn request_tokens_setup(Case(tokens, requests): Case) -> RequestTokens {
	let rt = runtime();
	let origin = unheld(&rt);
	let (client, server, pending, receive) = {
		let _guard = rt.enter();
		rt.block_on(publisher(&origin))
	};
	let announced = receive.consume().routes();
	let auths = rt.block_on(async {
		let mut auths = Vec::new();
		for token in 0..tokens {
			auths.push(added(&client.auth(), format!("token/{token}")).await.unwrap());
		}
		auths
	});
	RequestTokens {
		origin,
		_client: client,
		_server: server,
		pending,
		_receive: receive,
		announced,
		auths,
		generation: 0,
		requests,
		rt,
	}
}

#[bench(args = cases([1, 8, 32], [1, 8, 32]), setup = request_tokens_setup, items = |Case(tokens, requests)| tokens * requests)]
fn request_tokens(input: &mut RequestTokens) {
	let RequestTokens {
		origin,
		pending,
		announced,
		auths,
		generation,
		requests,
		rt,
		..
	} = input;
	let (tokens, requests) = (auths.len(), *requests);
	rt.block_on(async {
		let prefix = format!("round/{generation}");
		*generation += 1;
		let mut broadcasts = Vec::new();
		for (token, auth) in auths.iter().enumerate() {
			for request in 0..requests {
				broadcasts.push(
					origin
						.route(
							format!("{prefix}/{token}/{request}").as_str(),
							moq_net::origin::RouteOptions::default().with_auth(auth.clone()),
						)
						.unwrap(),
				);
			}
		}
		let issued = accept(pending, tokens).await;
		let mut seen = std::collections::HashSet::new();
		while seen.len() < tokens * requests {
			if let moq_net::origin::RouteEvent {
				kind: moq_net::origin::RouteKind::Start | moq_net::origin::RouteKind::Restart,
				announce: update,
				..
			} = announced.next().await.unwrap()
				&& update.prefix.as_str().starts_with(&prefix)
			{
				seen.insert(update.prefix);
			}
		}
		drop(broadcasts);
		join_all(issued.iter().map(auth::Issued::closed)).await;
	})
}

fn main() {
	swole::main();
}

/// Present `token` on `handle` and wait for its first verdict.
async fn added(
	handle: &moq_net::auth::Handle,
	token: impl Into<bytes::Bytes>,
) -> moq_net::Result<moq_net::auth::Credential> {
	let credential = handle.add(token)?;
	credential.answered().await?;
	Ok(credential)
}
