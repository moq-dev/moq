//! The configured tokens a connection holds on its auth handle, beside each dial's
//! connection credential.

use std::sync::{Arc, Mutex};
use std::task::Poll;

use bytes::Bytes;
use futures::FutureExt;
use moq_net::{auth, kio};

use crate::abort::AbortOnDrop;
use crate::connect::{Credential, Set, Token};

type Opening = futures::future::BoxFuture<'static, moq_net::Result<(auth::Auth, auth::Answer)>>;

/// Holds every configured token except the latest dial's connection credential on the
/// connection's auth handle, which presents them on each session it builds, and keeps
/// them in step with the client's [`crate::connect::Tokens`].
pub(super) struct Holder {
	shared: Arc<Shared>,
	_task: AbortOnDrop,
}

struct Shared {
	state: Mutex<Holding>,
	dialed: tokio::sync::Notify,
}

struct Holding {
	auth: auth::Handle,
	tokens: Vec<Token>,
	/// `None` until the first dial, so a token that becomes the connection credential is
	/// never also presented in band.
	credential: Option<Credential>,
	held: Held,
}

impl Holder {
	pub fn new(auth: auth::Handle, tokens: kio::Consumer<Set>) -> Self {
		let set = (*tokens.read()).clone();
		let shared = Arc::new(Shared {
			state: Mutex::new(Holding {
				auth,
				tokens: set.tokens,
				credential: None,
				held: Held::default(),
			}),
			dialed: tokio::sync::Notify::new(),
		});
		let task = tokio::spawn(run(shared.clone(), tokens, set.generation));
		Self {
			shared,
			_task: AbortOnDrop::new(task),
		}
	}

	/// Hold the tokens `credential` leaves over. Call it before the dial that presents
	/// `credential` starts: the new tokens register before this returns, so the session
	/// presents them from its start.
	pub fn dialed(&self, credential: &Credential) {
		let mut state = self.shared.state.lock().unwrap();
		state.credential = Some(credential.clone());
		state.held.unsupported = false;
		state.update();
		drop(state);
		self.shared.dialed.notify_one();
	}
}

impl Holding {
	fn update(&mut self) {
		let Some(credential) = &self.credential else { return };
		let extra = credential.extra(&self.tokens);
		self.held.update(&self.auth, extra);
	}
}

async fn run(shared: Arc<Shared>, tokens: kio::Consumer<Set>, mut generation: u64) {
	loop {
		let changed = async {
			let wait = tokens.wait(|set| match set.generation != generation {
				true => Poll::Ready((set.tokens.clone(), set.generation)),
				false => Poll::Pending,
			});
			// Every client handle dropped, so the tokens can no longer change.
			match wait.await {
				Ok(changed) => changed,
				Err(_) => std::future::pending().await,
			}
		};
		let answered = std::future::poll_fn(|cx| shared.state.lock().unwrap().held.poll_answers(cx));
		tokio::select! {
			biased;
			(latest, next) = changed => {
				generation = next;
				let mut state = shared.state.lock().unwrap();
				state.tokens = latest;
				state.update();
			}
			// A dial added tokens to wait on.
			_ = shared.dialed.notified() => {}
			_ = answered => {}
		}
	}
}

/// The configured tokens held on their own AUTH streams.
#[derive(Default)]
struct Held {
	tokens: Vec<(Token, Presented)>,
	/// Tokens a change replaced, answered or not, kept presented until every
	/// replacement has its answer so the grant never narrows in between.
	retiring: Vec<Presented>,
	/// Whether the latest dial already logged that its session carries no extra tokens.
	unsupported: bool,
}

enum Presented {
	Waiting(Opening),
	Granted(#[allow(dead_code, reason = "held to keep the token presented")] auth::Auth),
	/// Refused or unsupported. Not presented again unless a change removes and re-adds it.
	Refused,
}

impl Held {
	fn update(&mut self, handle: &auth::Handle, tokens: Vec<Token>) {
		let mut old = std::mem::take(&mut self.tokens);
		for token in tokens {
			let answer = match old.iter().position(|(held, _)| *held == token) {
				Some(index) => old.swap_remove(index).1,
				None => {
					let handle = handle.clone();
					let bytes = Bytes::from(token.as_str().to_string());
					let mut opening = async move { handle.add(bytes).await }.boxed();
					// The first poll registers the token synchronously.
					if let Some(result) = opening.as_mut().now_or_never() {
						opening = futures::future::ready(result).boxed();
					}
					Presented::Waiting(opening)
				}
			};
			self.tokens.push((token, answer));
		}
		// A waiting registration may already hold its grant, unread, so it retires too.
		self.retiring.extend(
			old.into_iter()
				.map(|(_, answer)| answer)
				.filter(|answer| !matches!(answer, Presented::Refused)),
		);
		self.settle();
	}

	/// Resolve when another token has its answer.
	fn poll_answers(&mut self, cx: &mut std::task::Context<'_>) -> Poll<()> {
		let mut answered = false;
		let mut unsupported = false;
		for (_, answer) in &mut self.tokens {
			let Presented::Waiting(opening) = answer else { continue };
			let Poll::Ready(result) = opening.as_mut().poll(cx) else {
				continue;
			};
			answered = true;
			*answer = match result {
				Ok((auth, answer)) => {
					// Without AUTH, moq-transport takes a token only for the requests that name
					// it, but a configured token rides no request, so this session sends it
					// nowhere. The handle still presents it on a later session that has AUTH.
					if answer == auth::Answer::Requests {
						unsupported = true;
					}
					Presented::Granted(auth)
				}
				Err(moq_net::Error::Unsupported) => {
					unsupported = true;
					Presented::Refused
				}
				Err(err) => {
					tracing::warn!(%err, "a configured token was refused");
					Presented::Refused
				}
			};
		}
		if unsupported && !std::mem::replace(&mut self.unsupported, true) {
			tracing::debug!("the peer takes no tokens in band; presenting only the connection credential");
		}
		self.settle();
		match answered {
			true => Poll::Ready(()),
			false => Poll::Pending,
		}
	}

	/// Withdraw the replaced tokens once nothing is waiting on an answer.
	fn settle(&mut self) {
		if !self
			.tokens
			.iter()
			.any(|(_, answer)| matches!(answer, Presented::Waiting(_)))
		{
			self.retiring.clear();
		}
	}
}

#[cfg(all(test, feature = "tcp"))]
mod tests {
	use super::*;

	struct Pair {
		_client: moq_net::Session,
		auth: auth::Handle,
		requests: tokio::sync::mpsc::UnboundedReceiver<auth::Request>,
		_tasks: Vec<AbortOnDrop<moq_net::Error>>,
		_acceptor: AbortOnDrop,
	}

	async fn pair_on(version: &str) -> Pair {
		let versions = moq_net::Versions::from(version.parse::<moq_net::Version>().unwrap());
		let config = || {
			let mut config = qmux::Config::new(qmux::Version::QMux01);
			config.protocol = qmux::Protocol::Negotiate(versions.alpns().iter().map(|alpn| alpn.to_string()).collect());
			config
		};
		let stream = |io| qmux::transport::Stream::new(io);
		let (client, server) = tokio::io::duplex(64 * 1024);
		let (client, server) = tokio::try_join!(
			qmux::Session::connect(stream(client), config()),
			qmux::Session::accept(stream(server), config())
		)
		.unwrap();
		let now = moq_net::time::Instant::now();
		let handle = auth::Handle::new();
		let client = async {
			let (session, driver) = moq_net::Client::new()
				.with_versions(versions.clone())
				.with_auth(handle.clone())
				.connect(now, client)
				.await
				.unwrap();
			(session, AbortOnDrop::new(tokio::spawn(moq_net::time::run(driver))))
		};
		let server = async {
			let mut request = moq_net::Server::new()
				.with_versions(versions.clone())
				.accept_request(now, server)
				.await
				.unwrap();
			let mut requests = request.requests();
			let (session, driver) = request.ok().await.unwrap();
			let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
			let acceptor = tokio::spawn(async move {
				let _session = session;
				let mut initial = Vec::new();
				while let Some(request) = requests.next().await {
					if request.token().is_empty() {
						initial.push(request.accept(auth::Grant::default()));
					} else if sender.send(request).is_err() {
						break;
					}
				}
			});
			(
				receiver,
				AbortOnDrop::new(tokio::spawn(moq_net::time::run(driver))),
				AbortOnDrop::new(acceptor),
			)
		};
		let ((client, client_task), (requests, server_task, acceptor)) = tokio::join!(client, server);
		Pair {
			_client: client,
			auth: handle,
			requests,
			_tasks: vec![client_task, server_task],
			_acceptor: acceptor,
		}
	}

	/// A holder for `tokens` on a pair's client, dialed with the credential a lite-07 dial
	/// to `url` would present.
	fn holder(pair: &Pair, url: &str, tokens: &[&str]) -> (crate::connect::Tokens, Holder) {
		let tokens = crate::connect::Tokens::new(tokens.iter().map(|token| token.parse().unwrap()).collect());
		let versions = moq_net::Versions::from("moq-lite-07-wip".parse::<moq_net::Version>().unwrap());
		let credential = Credential::new(&url.parse().unwrap(), &versions, &tokens.snapshot());
		let holder = Holder::new(pair.auth.clone(), tokens.consume());
		holder.dialed(&credential);
		(tokens, holder)
	}

	async fn next_token(pair: &mut Pair) -> auth::Request {
		moq_clock::timeout(std::time::Duration::from_secs(5), pair.requests.recv())
			.await
			.expect("no token presented")
			.unwrap()
	}

	/// Every configured token but the connection credential rides its own AUTH stream.
	#[tokio::test(start_paused = true)]
	async fn configured_tokens_ride_auth_beside_the_connection_credential() {
		let mut pair = pair_on("moq-lite-07-wip").await;
		let (_tokens, _holder) = holder(&pair, "moqt://relay.example.com/", &["connection", "a", "b"]);
		let mut presented = Vec::new();
		let mut issued = Vec::new();
		for _ in 0..2 {
			let request = next_token(&mut pair).await;
			presented.push(request.token().to_vec());
			issued.push(request.accept(auth::Grant::all()));
		}
		presented.sort();
		assert_eq!(presented, [b"a".to_vec(), b"b".to_vec()]);
		let again = moq_clock::timeout(std::time::Duration::from_secs(1), pair.requests.recv()).await;
		assert!(again.is_err(), "the connection credential was presented twice");
	}

	/// A URL that carries its own credential sends every configured token in band.
	#[tokio::test(start_paused = true)]
	async fn a_url_credential_sends_every_configured_token_in_band() {
		let mut pair = pair_on("moq-lite-07-wip").await;
		let (_tokens, _holder) = holder(&pair, "moqt://relay.example.com/?jwt=peer", &["a"]);
		let request = next_token(&mut pair).await;
		assert_eq!(request.token().as_ref(), b"a");
	}

	/// A replaced token is withdrawn only once its replacement has an answer, so the grant
	/// never narrows in between.
	#[tokio::test(start_paused = true)]
	async fn a_replaced_token_is_withdrawn_after_its_replacement_is_answered() {
		let mut pair = pair_on("moq-lite-07-wip").await;
		let (tokens, _holder) = holder(&pair, "moqt://relay.example.com/", &["connection", "old"]);
		let old = next_token(&mut pair).await.accept(auth::Grant::all());

		tokens.set(vec!["connection".parse().unwrap(), "new".parse().unwrap()]);
		let request = next_token(&mut pair).await;
		assert_eq!(request.token().as_ref(), b"new");
		let early = moq_clock::timeout(std::time::Duration::from_secs(1), old.closed()).await;
		assert!(early.is_err(), "withdrawn before its replacement was answered");

		let new = request.accept(auth::Grant::all());
		moq_clock::timeout(std::time::Duration::from_secs(5), old.closed())
			.await
			.expect("the replaced token was never withdrawn");
		let kept = moq_clock::timeout(std::time::Duration::from_secs(1), new.closed()).await;
		assert!(kept.is_err(), "the replacement was withdrawn");
	}

	/// A replacement is authoritative: the replaced token is withdrawn once the peer
	/// answers its replacement, even with a refusal.
	#[tokio::test(start_paused = true)]
	async fn a_refused_replacement_still_withdraws_the_replaced_token() {
		let mut pair = pair_on("moq-lite-07-wip").await;
		let (tokens, _holder) = holder(&pair, "moqt://relay.example.com/", &["connection", "old"]);
		let old = next_token(&mut pair).await.accept(auth::Grant::all());

		tokens.set(vec!["connection".parse().unwrap(), "new".parse().unwrap()]);
		let request = next_token(&mut pair).await;
		assert_eq!(request.token().as_ref(), b"new");
		request.reject(moq_net::SessionError::Unauthorized, "expired");
		moq_clock::timeout(std::time::Duration::from_secs(5), old.closed())
			.await
			.expect("the replaced token outlived its refused replacement");
	}

	/// Without AUTH a configured token rides nothing on this session, which logs that once,
	/// but the handle keeps it for a later session that has AUTH.
	#[tokio::test(start_paused = true)]
	async fn a_configured_token_without_auth_is_kept_for_a_later_session() {
		let pair = pair_on("moq-transport-14").await;
		let mut held = Held::default();
		held.update(&pair.auth, vec!["a".parse().unwrap(), "b".parse().unwrap()]);
		moq_clock::timeout(
			std::time::Duration::from_secs(5),
			std::future::poll_fn(|cx| held.poll_answers(cx)),
		)
		.await
		.expect("no answer");
		assert!(
			held.tokens
				.iter()
				.all(|(_, answer)| matches!(answer, Presented::Granted(_)))
		);
		assert!(held.unsupported);
	}
}
